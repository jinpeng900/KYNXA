use super::{ContractError, ContractResult, MAX_SAFE_INTEGER, hash, keys, member, safe_integer};
use serde_json::Value;

fn invalid() -> ContractError {
    ContractError::new(
        "Invalid document coverage. / 文档覆盖诊断无效。",
        "INVALID_RETRIEVAL_COVERAGE",
        400,
    )
}
fn integer(value: &Value, minimum: u64) -> bool {
    safe_integer(value, minimum, MAX_SAFE_INTEGER)
}
fn greater(left: &Value, right: &Value) -> bool {
    left.as_f64()
        .zip(right.as_f64())
        .is_some_and(|(left, right)| left > right)
}
fn signed_integer(value: &Value) -> bool {
    value.as_f64().is_some_and(|value| {
        value.is_finite() && value.fract() == 0.0 && value.abs() <= MAX_SAFE_INTEGER as f64
    })
}

pub fn validate(value: &Value) -> ContractResult {
    let reason = value["reasonCode"].as_str().unwrap_or("");
    if !keys(
        value,
        &[
            "format",
            "reasonCode",
            "complete",
            "processedPages",
            "pageCount",
            "publishedPages",
            "rawContentHash",
            "uncoveredRanges",
            "unprocessedRanges",
            "missingTextPages",
            "effectiveLimits",
            "limit",
            "readback",
        ],
    ) || !member(&value["format"], &["pdf", "docx"])
        || !(reason
            .strip_prefix("DOCUMENT_")
            .or_else(|| reason.strip_prefix("OCR_"))
            .is_some_and(|suffix| {
                !suffix.is_empty()
                    && suffix
                        .bytes()
                        .all(|byte| byte.is_ascii_uppercase() || byte == b'_')
            }))
        || value["complete"] != false
        || value["publishedPages"] != 0
        || !integer(&value["processedPages"], 0)
        || value
            .get("rawContentHash")
            .is_some_and(|value| !hash(value))
    {
        return Err(invalid());
    }
    if value.get("pageCount").is_some_and(|page_count| {
        !integer(page_count, 1) || value["processedPages"].as_f64() > page_count.as_f64()
    }) || value["format"] == "docx"
        && (value.get("pageCount").is_some() || value["processedPages"] != 0)
    {
        return Err(invalid());
    }
    for field in ["uncoveredRanges", "unprocessedRanges"] {
        if value[field].as_array().is_none_or(|ranges| {
            ranges.len() > 101
                || ranges.iter().any(|range| {
                    !keys(range, &["startPage", "endPage"])
                        || !integer(&range["startPage"], 1)
                        || !integer(&range["endPage"], 1)
                        || range["endPage"].as_f64() < range["startPage"].as_f64()
                        || value.get("pageCount").is_none()
                        || range["endPage"].as_f64() > value["pageCount"].as_f64()
                })
        }) {
            return Err(invalid());
        }
    }
    if value.get("missingTextPages").is_some_and(|pages| {
        pages.as_array().is_none_or(|pages| {
            pages.len() > 100
                || pages
                    .iter()
                    .any(|page| !integer(page, 1) || greater(page, &value["pageCount"]))
        })
    }) {
        return Err(invalid());
    }
    let limits = &value["effectiveLimits"];
    if !keys(
        limits,
        &["maximumInputBytes", "maximumOutputBytes", "maximumPages"],
    ) || ["maximumInputBytes", "maximumOutputBytes", "maximumPages"]
        .iter()
        .any(|field| !integer(&limits[field], 1))
    {
        return Err(invalid());
    }
    if value.get("limit").is_some_and(|limit| {
        !keys(limit, &["dimension", "limit", "observed"])
            || !member(
                &limit["dimension"],
                &[
                    "maximumInputBytes",
                    "maximumOutputBytes",
                    "maximumPages",
                    "maximumExpandedBytes",
                    "maximumArchiveEntries",
                ],
            )
            || !integer(&limit["limit"], 1)
            || !integer(&limit["observed"], 0)
    }) {
        return Err(invalid());
    }
    let readback = &value["readback"];
    if !keys(readback, &["kind", "startPage", "endPage"])
        || readback["kind"] != "original-document"
        || readback
            .get("startPage")
            .is_some_and(|start| !integer(start, 1))
        || readback.get("endPage").is_some_and(|end| {
            !signed_integer(end)
                || greater(&readback["startPage"], end)
                || greater(end, &value["pageCount"])
        })
    {
        return Err(invalid());
    }
    Ok(value.clone())
}

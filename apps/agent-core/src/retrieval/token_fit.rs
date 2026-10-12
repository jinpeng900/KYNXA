use serde_json::{Value, json};
use sha2::{Digest, Sha256};

use super::lexical::collect_terms;

const INVALID_FIT: &str = "INVALID_RETRIEVAL_TOKEN_FIT";

fn text_length(text: &str) -> usize {
    text.encode_utf16().count()
}
fn hash_text(text: &str) -> String {
    format!("{:x}", Sha256::digest(text.as_bytes()))
}
fn split_pair(text: &[u16], offset: usize) -> bool {
    offset > 0
        && offset < text.len()
        && (0xd800..=0xdbff).contains(&text[offset - 1])
        && (0xdc00..=0xdfff).contains(&text[offset])
}

fn projection(value: &Value) -> Result<Value, String> {
    let record = value.as_object().ok_or_else(|| INVALID_FIT.to_owned())?;
    let context = value["context"]
        .as_str()
        .ok_or_else(|| INVALID_FIT.to_owned())?;
    let version = value["version"]
        .as_str()
        .ok_or_else(|| INVALID_FIT.to_owned())?;
    let maximum = value["maxInputTokens"]
        .as_u64()
        .ok_or_else(|| INVALID_FIT.to_owned())?;
    let tokens = value["tokenCount"]
        .as_u64()
        .ok_or_else(|| INVALID_FIT.to_owned())?;
    if record
        .keys()
        .any(|key| !["context", "tokenCount", "maxInputTokens", "version"].contains(&key.as_str()))
        || text_length(context) > 256
        || context
            .chars()
            .any(|character| matches!(character as u32, 0..=8 | 11 | 12 | 14..=31))
        || version.is_empty()
        || text_length(version) > 128
        || version.chars().any(|character| (character as u32) <= 31)
        || !(1..=32768).contains(&maximum)
        || tokens < 1
        || tokens > maximum
    {
        return Err(INVALID_FIT.into());
    }
    Ok(json!({"context":context, "tokenCount":tokens, "maxInputTokens":maximum, "version":version}))
}

/// Token fitting changes only body ranges; metadata is never promoted into cited original text.
/// token 拟合只改变正文分块范围；投影元信息绝不升级成可引用原文。
pub fn apply(operation: &str, args: &Value) -> Result<Value, String> {
    if operation == "retrieval.token-projection" {
        return projection(&args["value"]);
    }
    if operation == "retrieval.fitted-text" {
        let fitted = projection(&args["chunk"]["embeddingProjection"])?;
        let text = args["chunk"]["text"]
            .as_str()
            .ok_or_else(|| INVALID_FIT.to_owned())?;
        let context = fitted["context"].as_str().unwrap_or("");
        if text_length(context) + text_length(text) > 16384 {
            return Err(INVALID_FIT.into());
        }
        return Ok(json!(format!("{context}{text}")));
    }
    if operation != "retrieval.token-fit" {
        return Err("AGENT_CORE_UNKNOWN_OPERATION".into());
    }
    let source = &args["source"];
    let chunk = &args["chunk"];
    let text = chunk["text"]
        .as_str()
        .ok_or_else(|| INVALID_FIT.to_owned())?;
    if args["projection"]["text"].as_str() != Some(text) {
        return Err(INVALID_FIT.into());
    }
    let segments = args["document"]["segments"]
        .as_array()
        .filter(|segments| !segments.is_empty())
        .ok_or_else(|| INVALID_FIT.to_owned())?;
    let units: Vec<u16> = text.encode_utf16().collect();
    let source_prefix = hash_text(
        source["sourceId"]
            .as_str()
            .ok_or_else(|| INVALID_FIT.to_owned())?,
    )[..24]
        .to_owned();
    let mut previous_end = 0;
    let mut start_line = chunk["startLine"]
        .as_u64()
        .ok_or_else(|| INVALID_FIT.to_owned())?;
    let source_start = chunk["startOffset"]
        .as_u64()
        .ok_or_else(|| INVALID_FIT.to_owned())?;
    let index_offset = args["chunkOffset"].as_u64().unwrap_or(0) as usize;
    let mut fitted = Vec::new();
    for segment in segments {
        let start = segment["start"]
            .as_u64()
            .ok_or_else(|| INVALID_FIT.to_owned())? as usize;
        let end = segment["end"]
            .as_u64()
            .ok_or_else(|| INVALID_FIT.to_owned())? as usize;
        if start != previous_end
            || end <= start
            || end > units.len()
            || split_pair(&units, start)
            || split_pair(&units, end)
        {
            return Err(INVALID_FIT.into());
        }
        let body = String::from_utf16(&units[start..end]).map_err(|_| INVALID_FIT.to_owned())?;
        let chunk_hash = hash_text(&body);
        let chunk_index = index_offset + fitted.len();
        let embedding = projection(
            &json!({"context":args["projection"]["context"], "tokenCount":segment["tokenCount"],
            "maxInputTokens":args["maxInputTokens"], "version":args["fittingVersion"]}),
        )?;
        let end_line =
            start_line + body.chars().filter(|character| *character == '\n').count() as u64;
        let lexical = collect_terms(
            &format!(
                "{} {} {}",
                source["title"].as_str().unwrap_or("undefined"),
                source["locator"]["relativePath"].as_str().unwrap_or(""),
                body
            ),
            20000,
            false,
        )
        .join(" ");
        let version = chunk["chunkerVersion"]
            .as_str()
            .unwrap_or("structure-lines-v1");
        fitted.push(json!({"chunkIndex":chunk_index, "chunkId":format!("{source_prefix}:{}:{chunk_index}", &chunk_hash[..24]),
            "text":body, "chunkHash":chunk_hash, "startOffset":source_start + start as u64, "endOffset":source_start + end as u64,
            "startLine":start_line, "endLine":end_line, "lexicalText":lexical,
            "chunkerVersion":format!("token-fitted-units-v1|{version}"), "embeddingProjection":embedding}));
        if index_offset + fitted.len() > 40000 {
            return Err("RETRIEVAL_SOURCE_TOO_LARGE".into());
        }
        start_line = end_line;
        previous_end = end;
    }
    if previous_end != units.len() {
        return Err(INVALID_FIT.into());
    }
    Ok(json!(fitted))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn tokens_cover_original_body_and_preserve_hash_and_lines() {
        let args = json!({"source":{"sourceId":"fit","title":"标题","locator":{}},
            "chunk":{"text":"😀\nword","startOffset":10,"startLine":3}, "projection":{"text":"😀\nword","context":"Title\n\n"},
            "document":{"segments":[{"start":0,"end":3,"tokenCount":4},{"start":3,"end":7,"tokenCount":2}]},
            "maxInputTokens":512,"fittingVersion":"v1"});
        let fitted = apply("retrieval.token-fit", &args).unwrap();
        assert_eq!(fitted[0]["text"], "😀\n");
        assert_eq!(fitted[1]["startOffset"], 13);
        assert_eq!(fitted[1]["startLine"], 4);
        assert_eq!(fitted[1]["chunkHash"], hash_text("word"));
    }
    #[test]
    fn invalid_fit_cannot_split_unicode_or_forge_budget() {
        assert_eq!(
            projection(
                &json!({"context":"", "version":"v1", "maxInputTokens":512,"tokenCount":513})
            ),
            Err(INVALID_FIT.into())
        );
        let result = apply(
            "retrieval.token-fit",
            &json!({"source":{"sourceId":"fit"}, "chunk":{"text":"😀","startOffset":0,"startLine":1},
            "projection":{"text":"😀","context":""}, "document":{"segments":[{"start":0,"end":1,"tokenCount":1}]},
            "maxInputTokens":512,"fittingVersion":"v1"}),
        );
        assert_eq!(result, Err(INVALID_FIT.into()));
    }
}

use std::sync::OnceLock;

use regex::Regex;
use serde_json::{Value, json};

const INVALID_STRUCTURE: &str = "INVALID_RETRIEVAL_STRUCTURE";

pub fn split_pair(text: &[u16], offset: usize) -> bool {
    offset > 0
        && offset < text.len()
        && (0xd800..=0xdbff).contains(&text[offset - 1])
        && (0xdc00..=0xdfff).contains(&text[offset])
}

pub fn whitespace(character: u16) -> bool {
    matches!(character, 0x0009..=0x000d | 0x0020 | 0x00a0 | 0x1680 | 0x2000..=0x200a | 0x2028 | 0x2029 | 0x202f | 0x205f | 0x3000 | 0xfeff)
}

fn byte_offsets(text: &str) -> Vec<usize> {
    let mut offsets = vec![0; text.len() + 1];
    let mut offset = 0;
    for (byte, character) in text.char_indices() {
        offsets[byte] = offset;
        offset += character.len_utf16();
        offsets[byte + character.len_utf8()] = offset;
    }
    offsets
}

/// Use UTF-16 offsets while preserving the original text for slicing and hashing.
/// 偏移使用 UTF-16 单位，切片与哈希保留原始文本。
fn plain_chunks(raw_text: &str, text: &[u16], maximum: usize) -> Vec<Value> {
    static BOUNDARY: OnceLock<Regex> = OnceLock::new();
    static HEADINGS: OnceLock<Regex> = OnceLock::new();
    let offsets = byte_offsets(raw_text);
    let mut starts = vec![0];
    for found in BOUNDARY.get_or_init(|| Regex::new(r"(?m)^#{1,6}\s+|^(?:export\s+)?(?:async\s+)?(?:function|class|interface|def)\s+|\n\s*\n").expect("static chunk boundaries")).find_iter(raw_text) {
        let offset=offsets[found.start()];if offset>*starts.last().unwrap_or(&0) {starts.push(offset);}
    }
    starts.push(text.len());
    let headings = HEADINGS
        .get_or_init(|| {
            Regex::new(r"(?m)^#{1,6}\s+([^\r\n\x{2028}\x{2029}]+)")
                .expect("static section headings")
        })
        .captures_iter(raw_text)
        .map(|capture| {
            (
                offsets[capture.get(0).expect("heading match").start()],
                capture[1].trim().to_owned(),
            )
        })
        .collect::<Vec<_>>();
    let mut chunks = Vec::new();
    let mut heading_index = 0;
    let mut section: Option<String> = None;
    let mut start = 0;
    let mut start_line = 1;
    let mut append = |end: usize, start: &mut usize, start_line: &mut usize| {
        if end <= *start {
            return;
        }
        let content = &text[*start..end];
        let end_line = *start_line + content.iter().filter(|character| **character == 10).count();
        if let Some(nonwhite) = content.iter().position(|character| !whitespace(*character)) {
            while heading_index < headings.len() && headings[heading_index].0 <= *start + nonwhite {
                section = Some(headings[heading_index].1.clone());
                heading_index += 1;
            }
            chunks.push(json!({"startOffset":*start,"endOffset":end,"startLine":*start_line,"endLine":end_line,"sectionTitle":section}));
        }
        *start = end;
        *start_line = end_line;
    };
    for boundary in starts {
        while boundary - start > maximum {
            let mut end = start + maximum;
            if (0xd800..=0xdbff).contains(&text[end - 1]) {
                end -= 1;
            }
            let newline = text[..=end.min(text.len().saturating_sub(1))]
                .iter()
                .rposition(|character| *character == 10);
            append(
                newline
                    .filter(|newline| *newline as f64 > start as f64 + maximum as f64 / 2.0)
                    .map_or(end, |newline| newline + 1),
                &mut start,
                &mut start_line,
            );
        }
        if (boundary - start) as f64 > maximum as f64 / 2.0 {
            append(boundary, &mut start, &mut start_line);
        }
    }
    append(text.len(), &mut start, &mut start_line);
    chunks
}

fn valid_offset(value: &Value, maximum: usize) -> Option<usize> {
    value
        .as_u64()
        .filter(|value| *value <= maximum as u64)
        .map(|value| value as usize)
}

fn unit_range(unit: &Value, text: &[u16]) -> Result<(usize, usize), String> {
    let start = valid_offset(&unit["startOffset"], text.len()).ok_or(INVALID_STRUCTURE)?;
    let end = valid_offset(&unit["endOffset"], text.len()).ok_or(INVALID_STRUCTURE)?;
    if end <= start || split_pair(text, start) || split_pair(text, end) {
        return Err(INVALID_STRUCTURE.into());
    }
    Ok((start, end))
}

fn metadata(unit: &Value, structure: &Value, text: &[u16]) -> Result<Value, String> {
    static KIND: OnceLock<Regex> = OnceLock::new();
    let (start, end) = unit_range(unit, text)?;
    if !unit["kind"].as_str().is_some_and(|kind| {
        KIND.get_or_init(|| Regex::new(r"^[a-z][a-z-]{0,63}$").expect("static structural kind"))
            .is_match(kind)
    }) {
        return Err(INVALID_STRUCTURE.into());
    }
    let unit_start = unit
        .get("unitStartOffset")
        .map_or(Some(start), |value| valid_offset(value, text.len()))
        .ok_or(INVALID_STRUCTURE)?;
    let unit_end = unit
        .get("unitEndOffset")
        .map_or(Some(end), |value| valid_offset(value, text.len()))
        .ok_or(INVALID_STRUCTURE)?;
    if unit_start > start
        || unit_end < end
        || unit_end <= unit_start
        || split_pair(text, unit_start)
        || split_pair(text, unit_end)
    {
        return Err(INVALID_STRUCTURE.into());
    }
    let mut metadata = json!({"domain":structure["domain"],"language":structure["language"],"kind":unit["kind"],"unitStartOffset":unit_start,"unitEndOffset":unit_end,"parseStatus":structure["parseStatus"]});
    for key in [
        "symbolName",
        "qualifiedName",
        "parentSymbol",
        "sectionTitle",
    ] {
        if let Some(value) = unit.get(key) {
            let label = value
                .as_str()
                .filter(|label| {
                    label.encode_utf16().count() <= 512
                        && !label.chars().any(|character| character < ' ')
                })
                .ok_or(INVALID_STRUCTURE)?;
            if !label.encode_utf16().all(whitespace) {
                metadata[key] = value.clone();
            }
        }
    }
    if let Some(path) = unit.get("sectionPath") {
        if !path.as_array().is_some_and(|items| {
            items.len() <= 6
                && items.iter().all(|value| {
                    value.as_str().is_some_and(|label| {
                        label.encode_utf16().count() <= 512
                            && !label.chars().any(|character| character < ' ')
                    })
                })
        }) {
            return Err(INVALID_STRUCTURE.into());
        }
        metadata["sectionPath"] = path.clone();
    }
    Ok(metadata)
}

fn structured_chunks(
    structure: &Value,
    text: &[u16],
    maximum: usize,
) -> Result<Vec<Value>, String> {
    if !matches!(structure["domain"].as_str(), Some("code" | "knowledge"))
        || !(structure["language"].is_null()
            || structure["language"]
                .as_str()
                .is_some_and(|language| language.encode_utf16().count() <= 64))
        || !matches!(
            structure["parseStatus"].as_str(),
            Some("parsed" | "partial" | "unavailable")
        )
    {
        return Err(INVALID_STRUCTURE.into());
    }
    let units = structure["units"].as_array().ok_or(INVALID_STRUCTURE)?;
    if units.iter().any(|unit| !unit.is_object()) {
        return Err(INVALID_STRUCTURE.into());
    }
    let mut units = units.iter().cloned().collect::<Vec<_>>();
    units.sort_by_key(|unit| {
        (
            unit["startOffset"].as_u64().unwrap_or(0),
            unit["endOffset"].as_u64().unwrap_or(0),
        )
    });
    let mut cursor = 0;
    let mut line_offset = 0;
    let mut line = 1;
    let mut chunks = Vec::new();
    let mut append = |unit: &Value| -> Result<(), String> {
        let (mut start, unit_end) = unit_range(unit, text)?;
        let metadata = metadata(unit, structure, text)?;
        let line_at = |offset: usize, line_offset: &mut usize, line: &mut usize| {
            while *line_offset < offset {
                if text[*line_offset] == 10 {
                    *line += 1;
                }
                *line_offset += 1;
            }
            *line
        };
        let start_line = line_at(start, &mut line_offset, &mut line);
        if unit
            .get("startLine")
            .is_some_and(|value| value.as_u64() != Some(start_line as u64))
        {
            return Err(INVALID_STRUCTURE.into());
        }
        while start < unit_end {
            let mut end = unit_end.min(start + maximum);
            if split_pair(text, end) {
                end -= 1;
            }
            if end < unit_end {
                if let Some(newline) = text[start..end]
                    .iter()
                    .rposition(|character| *character == 10)
                    .filter(|newline| *newline as f64 > maximum as f64 / 2.0)
                {
                    end = start + newline + 1;
                }
            }
            let start_line = line_at(start, &mut line_offset, &mut line);
            let end_line = line_at(end, &mut line_offset, &mut line);
            if text[start..end]
                .iter()
                .any(|character| !whitespace(*character))
            {
                chunks.push(json!({"startOffset":start,"endOffset":end,"startLine":start_line,"endLine":end_line,"structure":metadata}));
            }
            start = end;
        }
        if unit.get("endLine").is_some_and(|value| {
            value.as_u64() != Some(line_at(unit_end, &mut line_offset, &mut line) as u64)
        }) {
            return Err(INVALID_STRUCTURE.into());
        }
        Ok(())
    };
    for unit in units {
        let (start, end) = unit_range(&unit, text)?;
        if start < cursor {
            return Err(INVALID_STRUCTURE.into());
        }
        if start > cursor {
            append(&json!({"kind":"context","startOffset":cursor,"endOffset":start}))?;
        }
        append(&unit)?;
        cursor = end;
    }
    if cursor < text.len() {
        append(&json!({"kind":"context","startOffset":cursor,"endOffset":text.len()}))?;
    }
    Ok(chunks)
}

pub fn apply(args: &Value) -> Result<Value, String> {
    let raw_text = args["text"].as_str().ok_or("INVALID_RETRIEVAL_SOURCE")?;
    let text = raw_text.encode_utf16().collect::<Vec<_>>();
    if text.len() > 2 * 1024 * 1024 {
        return Err("RETRIEVAL_SOURCE_TOO_LARGE".into());
    }
    let maximum = args["maxChars"]
        .as_u64()
        .filter(|maximum| (64..=8000).contains(maximum))
        .ok_or("INVALID_RETRIEVAL_CHUNK_SIZE")? as usize;
    match args.get("structure") {
        Some(structure) => Ok(json!(structured_chunks(structure, &text, maximum)?)),
        None => Ok(json!(plain_chunks(raw_text, &text, maximum))),
    }
}

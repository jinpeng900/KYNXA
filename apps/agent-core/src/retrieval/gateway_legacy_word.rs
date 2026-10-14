//! Word 97+ text follows the FIB, live CLX pieces and declared story CP ranges.
//! Word 97+ 正文按 FIB、活动 CLX 片段及故事 CP 范围读取，不扫描残留历史字符串。
//!
//! Format references: MS-DOC §§ 2.3, 2.5.1–2.5.15, 2.8.35, 2.9.38 and 2.9.73.
//! 格式依据为上述微软公开结构；压缩片段是八位 Unicode 与规定映射，不按安装语言猜编码。

use super::{CompoundDocument, range, u16_at, u32_at};
use crate::{
    retrieval::gateway_office_documents::{DocumentText, cancelled, failure},
    storage::ContractError,
};
use serde_json::{Value, json};
use std::sync::{Arc, atomic::AtomicBool};

const STORY_FIELDS: [(usize, &str); 7] = [
    (3, "body"),
    (4, "footnotes"),
    (5, "headers-footers"),
    (7, "comments"),
    (8, "endnotes"),
    (9, "textboxes"),
    (10, "header-textboxes"),
];

struct WordFib {
    version: u16,
    table_one: bool,
    meaningful_bytes: usize,
    minimum_text_offset: usize,
    stories: [(usize, &'static str); 7],
    character_count: usize,
    fc_clx: usize,
    lcb_clx: usize,
}

/// Counts navigate the variable-length FIB; unknown generations are never guessed.
/// 根据计数跳过变长 FIB 子结构；未知代际明确拒绝，不把截短头部当成零值继续。
fn read_fib(word: &[u8], limits: &Value) -> Result<WordFib, ContractError> {
    if u16_at(word, 0)? != 0xa5ec {
        return Err(failure("DOCUMENT_PARSE_FAILED"));
    }
    let base_version = u16_at(word, 2)?;
    if base_version < 0x00c1 {
        return Err(failure("DOCUMENT_LEGACY_WORD_VERSION_UNSUPPORTED"));
    }
    let flags = u16_at(word, 10)?;
    if flags & 0x0100 != 0 {
        return Err(failure("DOCUMENT_ENCRYPTED_UNSUPPORTED"));
    }
    if word.get(18).copied().unwrap_or(1) != 0 || word.get(19).copied().unwrap_or(1) & 1 != 0 {
        return Err(failure("DOCUMENT_LEGACY_WORD_VERSION_UNSUPPORTED"));
    }
    let csw = u16_at(word, 32)? as usize;
    let cslw_position = 34 + csw * 2;
    let cslw = u16_at(word, cslw_position)? as usize;
    if csw != 14 || cslw < 22 {
        return Err(failure("DOCUMENT_PARSE_FAILED"));
    }
    let long_fields = cslw_position + 2;
    range(word, long_fields, cslw * 4)?;
    let pair_count_position = long_fields + cslw * 4;
    let pair_count = u16_at(word, pair_count_position)? as usize;
    let pairs = pair_count_position + 2;
    if pair_count < 93 {
        return Err(failure("DOCUMENT_PARSE_FAILED"));
    }
    range(word, pairs, pair_count * 8)?;
    let new_count_position = pairs + pair_count * 8;
    let new_count = u16_at(word, new_count_position)? as usize;
    range(word, new_count_position + 2, new_count * 2)?;
    let version = if new_count == 0 {
        base_version
    } else {
        u16_at(word, new_count_position + 2)?
    };
    if !matches!(version, 0x00c1 | 0x00d9 | 0x0101 | 0x010c | 0x0112) {
        return Err(failure("DOCUMENT_LEGACY_WORD_VERSION_UNSUPPORTED"));
    }
    let meaningful_bytes = u32_at(word, long_fields)? as usize;
    if meaningful_bytes < new_count_position + 2 + new_count * 2 || meaningful_bytes > word.len() {
        return Err(failure("DOCUMENT_PARSE_FAILED"));
    }
    let mut stories = [(0, ""); 7];
    let mut total_characters = 0usize;
    for (index, (field, name)) in STORY_FIELDS.iter().copied().enumerate() {
        let count = u32_at(word, long_fields + field * 4)? as usize;
        if count >= 0x7fff_ffff {
            return Err(failure("DOCUMENT_PARSE_FAILED"));
        }
        total_characters = total_characters
            .checked_add(count)
            .ok_or_else(|| failure("DOCUMENT_STRUCTURE_LIMIT"))?;
        stories[index] = (count, name);
    }
    if stories[0].0 == 0 {
        return Err(failure("DOCUMENT_TEXT_UNAVAILABLE"));
    }
    // Extra final paragraph lies outside every story and is not displayed or indexed.
    // 其他故事存在时最后额外段落位于所有故事之外，不显示、不索引。
    let character_count =
        total_characters + usize::from(stories[1..].iter().any(|(count, _)| *count != 0));
    let maximum_characters = limits["maximumDocumentCharacters"]
        .as_u64()
        .unwrap_or(32 * 1024 * 1024)
        .clamp(1, 64 * 1024 * 1024) as usize;
    if character_count > maximum_characters {
        return Err(failure("DOCUMENT_STRUCTURE_LIMIT"));
    }
    let fc_clx = u32_at(word, pairs + 33 * 8)? as usize;
    let lcb_clx = u32_at(word, pairs + 33 * 8 + 4)? as usize;
    if lcb_clx == 0 {
        return Err(failure("DOCUMENT_PARSE_FAILED"));
    }
    Ok(WordFib {
        version,
        table_one: flags & 0x0200 != 0,
        meaningful_bytes,
        minimum_text_offset: new_count_position + 2 + new_count * 2,
        stories,
        character_count,
        fc_clx,
        lcb_clx,
    })
}

fn piece_table<'a>(
    clx: &'a [u8],
    limits: &Value,
    cancel: &Arc<AtomicBool>,
) -> Result<&'a [u8], ContractError> {
    let mut position = 0usize;
    let mut properties = 0usize;
    while clx.get(position) == Some(&1) {
        cancelled(cancel)?;
        let count = u16_at(clx, position + 1)? as usize;
        properties += 1;
        if count > 0x3fa2 || properties > 32768 {
            return Err(failure("DOCUMENT_STRUCTURE_LIMIT"));
        }
        range(clx, position + 3, count)?;
        position += 3 + count;
    }
    if clx.get(position) != Some(&2) {
        return Err(failure("DOCUMENT_PARSE_FAILED"));
    }
    let count = u32_at(clx, position + 1)? as usize;
    let table = range(clx, position + 5, count)?;
    if position + 5 + count != clx.len() || count < 4 || (count - 4) % 12 != 0 {
        return Err(failure("DOCUMENT_PARSE_FAILED"));
    }
    let piece_count = (count - 4) / 12;
    let maximum_pieces = limits["maximumDocumentPieces"]
        .as_u64()
        .unwrap_or(1_048_576)
        .clamp(1, 1_048_576) as usize;
    if piece_count == 0 || piece_count > maximum_pieces {
        return Err(failure("DOCUMENT_STRUCTURE_LIMIT"));
    }
    Ok(table)
}

/// MS-DOC compressed Unicode deliberately differs from a guessed ANSI/GBK codepage.
/// MS-DOC 八位压缩 Unicode 使用固定映射；中文正文应来自 UTF-16 片段，不能按 GBK 猜解。
fn compressed_unit(byte: u8) -> u16 {
    match byte {
        0x82 => 0x201a,
        0x83 => 0x0192,
        0x84 => 0x201e,
        0x85 => 0x2026,
        0x86 => 0x2020,
        0x87 => 0x2021,
        0x88 => 0x02c6,
        0x89 => 0x2030,
        0x8a => 0x0160,
        0x8b => 0x2039,
        0x8c => 0x0152,
        0x91 => 0x2018,
        0x92 => 0x2019,
        0x93 => 0x201c,
        0x94 => 0x201d,
        0x95 => 0x2022,
        0x96 => 0x2013,
        0x97 => 0x2014,
        0x98 => 0x02dc,
        0x99 => 0x2122,
        0x9a => 0x0161,
        0x9b => 0x203a,
        0x9c => 0x0153,
        0x9f => 0x0178,
        _ => byte as u16,
    }
}

fn read_characters(
    word: &[u8],
    table: &[u8],
    fib: &WordFib,
    cancel: &Arc<AtomicBool>,
) -> Result<Vec<u16>, ContractError> {
    let pieces = (table.len() - 4) / 12;
    if u32_at(table, 0)? != 0 || u32_at(table, pieces * 4)? as usize != fib.character_count {
        return Err(failure("DOCUMENT_PARSE_FAILED"));
    }
    let descriptors = (pieces + 1) * 4;
    let mut units = Vec::with_capacity(fib.character_count.min(65536));
    for index in 0..pieces {
        cancelled(cancel)?;
        let first = u32_at(table, index * 4)? as usize;
        let end = u32_at(table, (index + 1) * 4)? as usize;
        if first != units.len() || end <= first || end > fib.character_count {
            return Err(failure("DOCUMENT_PARSE_FAILED"));
        }
        let count = end - first;
        let location = u32_at(table, descriptors + index * 8 + 2)?;
        let compressed = location & 0x4000_0000 != 0;
        let raw_offset = (location & 0x3fff_ffff) as usize;
        let offset = if compressed {
            raw_offset / 2
        } else {
            raw_offset
        };
        let byte_count = count
            .checked_mul(if compressed { 1 } else { 2 })
            .ok_or_else(|| failure("DOCUMENT_PARSE_FAILED"))?;
        let contents = range(word, offset, byte_count)?;
        if offset < fib.minimum_text_offset
            || offset + byte_count > fib.meaningful_bytes
            || (compressed && raw_offset % 2 != 0)
        {
            return Err(failure("DOCUMENT_PARSE_FAILED"));
        }
        for chunk in contents.chunks(65536) {
            cancelled(cancel)?;
            if compressed {
                units.extend(chunk.iter().copied().map(compressed_unit));
            } else {
                units.extend(
                    chunk
                        .chunks_exact(2)
                        .map(|raw| u16::from_le_bytes([raw[0], raw[1]])),
                );
            }
        }
    }
    if units.len() != fib.character_count {
        return Err(failure("DOCUMENT_PARSE_FAILED"));
    }
    if fib.stories[1..].iter().any(|(count, _)| *count != 0) && units.last() != Some(&0x000d) {
        return Err(failure("DOCUMENT_PARSE_FAILED"));
    }
    Ok(units)
}

/// Only cached field results are projected; no field instruction, macro or link is executed.
/// 仅投影已经保存的域结果；不执行域指令、宏或链接，也不把指令当成正文证据。
fn append_story(
    output: &mut DocumentText,
    units: &[u16],
    cancel: &Arc<AtomicBool>,
) -> Result<bool, ContractError> {
    let mut field_results = Vec::<bool>::new();
    let mut text = String::new();
    let mut has_indexable_text = false;
    for (index, character) in char::decode_utf16(units.iter().copied()).enumerate() {
        if index % 4096 == 0 {
            cancelled(cancel)?;
        }
        let character = character.map_err(|_| failure("DOCUMENT_TEXT_ENCODING_UNSUPPORTED"))?;
        match character {
            '\u{0013}' => {
                if field_results.len() >= 128 {
                    return Err(failure("DOCUMENT_STRUCTURE_LIMIT"));
                }
                field_results.push(false);
            }
            '\u{0014}' => {
                let result = field_results
                    .last_mut()
                    .ok_or_else(|| failure("DOCUMENT_PARSE_FAILED"))?;
                if *result {
                    return Err(failure("DOCUMENT_PARSE_FAILED"));
                }
                *result = true;
            }
            '\u{0015}' => {
                field_results
                    .pop()
                    .ok_or_else(|| failure("DOCUMENT_PARSE_FAILED"))?;
            }
            _ if field_results.iter().any(|result| !result) => {}
            '\u{0007}' => text.push('\t'),
            '\r' | '\u{000b}' | '\u{000c}' | '\u{000e}' => text.push('\n'),
            '\t' | '\n' => text.push(character),
            '\u{001e}' => text.push('\u{2011}'),
            '\u{001f}' => {}
            _ if character.is_control() => {}
            _ => {
                has_indexable_text |= !character.is_whitespace();
                text.push(character);
            }
        }
        if text.len() >= 8192 {
            output.append(&text)?;
            text.clear();
        }
    }
    if !field_results.is_empty() {
        return Err(failure("DOCUMENT_PARSE_FAILED"));
    }
    output.append(&text)?;
    Ok(has_indexable_text)
}

pub(super) fn decode(
    bytes: &[u8],
    limits: &Value,
    cancel: &Arc<AtomicBool>,
    digest: &str,
) -> Result<Value, ContractError> {
    let mut compound = CompoundDocument::open(bytes, limits, cancel)?;
    let word = compound.read("/WordDocument", cancel)?;
    let fib = read_fib(&word, limits)?;
    let table = compound.read(if fib.table_one { "/1Table" } else { "/0Table" }, cancel)?;
    let clx = range(&table, fib.fc_clx, fib.lcb_clx)?;
    let pieces = piece_table(clx, limits, cancel)?;
    let units = read_characters(&word, pieces, &fib, cancel)?;
    let mut output = DocumentText::new(limits);
    let mut start_cp = 0usize;
    let mut has_indexable_text = false;
    for (count, name) in fib.stories {
        cancelled(cancel)?;
        let end_cp = start_cp + count;
        if count != 0 {
            let label = format!("{name} (CP {start_cp}..{end_cp})");
            let start = output.start_part("word-story", &label)?;
            has_indexable_text |= append_story(&mut output, &units[start_cp..end_cp], cancel)?;
            output.end_part("word-story", &label, start);
        }
        start_cp = end_cp;
    }
    cancelled(cancel)?;
    if !has_indexable_text {
        return Err(failure("DOCUMENT_TEXT_UNAVAILABLE"));
    }
    let mut result = output.finish(
        "doc",
        "rust-msdoc-text-v1",
        digest,
        &[
            "page-layout",
            "embedded-images",
            "embedded-objects",
            "macros",
            "external-links",
            "field-instructions",
            "formatting-dependent-visibility",
            "revision-display-policy",
            "automatic-list-labels",
            "individual-note-and-header-associations",
        ],
    )?;
    result["extraction"]["capabilities"] = json!([
        "text",
        "structure",
        "live-piece-table",
        "story-ranges",
        "cached-field-results"
    ]);
    result["legacyVersion"] = json!(fib.version);
    Ok(result)
}

#[cfg(test)]
#[path = "gateway_legacy_word_tests.rs"]
mod tests;

//! MS-PPT text follows the latest persist directory and the live presentation slide list.
//! 旧 PPT 正文遵循最新持久对象目录与活动幻灯片清单，不读取已删除版本中的任意字符串。

use super::{CompoundDocument, decode_utf16, range, u16_at, u32_at};
use crate::{
    retrieval::gateway_office_documents::{DocumentText, cancelled, failure},
    storage::ContractError,
};
use serde_json::Value;
use std::{
    collections::{HashMap, HashSet},
    sync::{Arc, atomic::AtomicBool},
};

const DOCUMENT: u16 = 0x03e8;
const SLIDE: u16 = 0x03ee;
const SLIDE_ATOM: u16 = 0x03ef;
const NOTES: u16 = 0x03f0;
const NOTES_ATOM: u16 = 0x03f1;
const SLIDE_PERSIST: u16 = 0x03f3;
const DRAWING: u16 = 0x040c;
const OUTLINE_REF: u16 = 0x0f9e;
const TEXT_HEADER: u16 = 0x0f9f;
const TEXT_CHARS: u16 = 0x0fa0;
const TEXT_BYTES: u16 = 0x0fa8;
const SLIDE_LIST: u16 = 0x0ff0;
const USER_EDIT: u16 = 0x0ff5;
const CURRENT_USER: u16 = 0x0ff6;
const PERSIST_DIRECTORY: u16 = 0x1772;
const ART_DRAWING: u16 = 0xf002;
const ART_GROUP: u16 = 0xf003;
const ART_SHAPE: u16 = 0xf004;
const ART_SHAPE_FLAGS: u16 = 0xf00a;
const ART_TEXTBOX: u16 = 0xf00d;

#[derive(Clone, Copy)]
struct Record<'a> {
    version: u16,
    instance: u16,
    kind: u16,
    payload: &'a [u8],
    end_offset_bytes: usize,
}

fn record_at(bytes: &[u8], offset_bytes: usize) -> Result<Record<'_>, ContractError> {
    let header = range(bytes, offset_bytes, 8)?;
    let flags = u16_at(header, 0)?;
    let length_bytes = u32_at(header, 4)? as usize;
    let body_offset = offset_bytes
        .checked_add(8)
        .ok_or_else(|| failure("DOCUMENT_PARSE_FAILED"))?;
    let payload = range(bytes, body_offset, length_bytes)?;
    Ok(Record {
        version: flags & 15,
        instance: flags >> 4,
        kind: u16_at(header, 2)?,
        payload,
        end_offset_bytes: body_offset + length_bytes,
    })
}

fn expect_record(
    record: Record<'_>,
    kind: u16,
    version: u16,
    length: Option<usize>,
) -> Result<(), ContractError> {
    if record.kind != kind
        || record.version != version
        || record.instance != 0
        || length.is_some_and(|length| record.payload.len() != length)
    {
        return Err(failure("DOCUMENT_PARSE_FAILED"));
    }
    Ok(())
}

struct ParseBudget<'a> {
    cancel: &'a Arc<AtomicBool>,
    used_records: usize,
    maximum_records: usize,
}

impl ParseBudget<'_> {
    fn next<'a>(
        &mut self,
        bytes: &'a [u8],
        offset: &mut usize,
    ) -> Result<Option<Record<'a>>, ContractError> {
        if *offset == bytes.len() {
            return Ok(None);
        }
        cancelled(self.cancel)?;
        self.used_records += 1;
        if self.used_records > self.maximum_records {
            return Err(failure("DOCUMENT_STRUCTURE_LIMIT"));
        }
        let record = record_at(bytes, *offset)?;
        *offset = record.end_offset_bytes;
        Ok(Some(record))
    }
}

/// Newer user edits win; a valid old mapping does not make a deleted slide live.
/// 新编辑的映射优先；旧目录仍含某对象，并不意味着已删除幻灯片仍属于当前文稿。
fn persist_directory(
    stream: &[u8],
    current: &[u8],
    budget: &mut ParseBudget<'_>,
) -> Result<(HashMap<u32, usize>, u32), ContractError> {
    let current = record_at(current, 0)?;
    expect_record(current, CURRENT_USER, 0, None)?;
    if current.payload.len() < 24 || u32_at(current.payload, 0)? != 20 {
        return Err(failure("DOCUMENT_PARSE_FAILED"));
    }
    match u32_at(current.payload, 4)? {
        0xf3d1c4df => return Err(failure("DOCUMENT_ENCRYPTED")),
        0xe391c05f => {}
        _ => return Err(failure("DOCUMENT_PARSE_FAILED")),
    }
    if u16_at(current.payload, 14)? != 0x03f4 || current.payload[16..18] != [3, 0] {
        return Err(failure("DOCUMENT_LEGACY_VERSION_UNSUPPORTED"));
    }
    let username_bytes = u16_at(current.payload, 12)? as usize;
    if username_bytes > 255 || current.payload.len() < 24 + username_bytes {
        return Err(failure("DOCUMENT_PARSE_FAILED"));
    }
    let mut edit_offset = u32_at(current.payload, 8)? as usize;
    let mut visited_edits = HashSet::new();
    let mut directory = HashMap::new();
    let mut document_id = None;
    loop {
        cancelled(budget.cancel)?;
        if visited_edits.len() >= 4096 || !visited_edits.insert(edit_offset) {
            return Err(failure("DOCUMENT_STRUCTURE_LIMIT"));
        }
        let edit = record_at(stream, edit_offset)?;
        expect_record(edit, USER_EDIT, 0, None)?;
        if ![28, 32].contains(&edit.payload.len()) || edit.payload[6..8] != [0, 3] {
            return Err(failure("DOCUMENT_PARSE_FAILED"));
        }
        if edit.payload.len() == 32 && u32_at(edit.payload, 28)? != 0 {
            return Err(failure("DOCUMENT_ENCRYPTED"));
        }
        let previous = u32_at(edit.payload, 8)? as usize;
        let directory_offset = u32_at(edit.payload, 12)? as usize;
        let current_document_id = u32_at(edit.payload, 16)?;
        if current_document_id != 1
            || (previous != 0 && previous >= edit_offset)
            || directory_offset <= previous
            || directory_offset >= edit_offset
        {
            return Err(failure("DOCUMENT_PARSE_FAILED"));
        }
        document_id.get_or_insert(current_document_id);
        let persist = record_at(stream, directory_offset)?;
        expect_record(persist, PERSIST_DIRECTORY, 0, None)?;
        if persist.end_offset_bytes > edit_offset {
            return Err(failure("DOCUMENT_PARSE_FAILED"));
        }
        let mut offset = 0;
        let mut current_ids = HashSet::new();
        while offset < persist.payload.len() {
            cancelled(budget.cancel)?;
            let entry = u32_at(persist.payload, offset)?;
            offset += 4;
            let first_id = entry & 0x000f_ffff;
            let count = entry >> 20;
            if first_id == 0 || count == 0 || first_id + count > 0x0010_0000 {
                return Err(failure("DOCUMENT_PARSE_FAILED"));
            }
            range(persist.payload, offset, count as usize * 4)?;
            for index in 0..count {
                if !current_ids.insert(first_id + index) {
                    return Err(failure("DOCUMENT_PARSE_FAILED"));
                }
                let object_offset = u32_at(persist.payload, offset)? as usize;
                offset += 4;
                if object_offset >= directory_offset {
                    return Err(failure("DOCUMENT_PARSE_FAILED"));
                }
                directory.entry(first_id + index).or_insert(object_offset);
                if directory.len() > budget.maximum_records {
                    return Err(failure("DOCUMENT_STRUCTURE_LIMIT"));
                }
            }
        }
        if previous == 0 {
            break;
        }
        edit_offset = previous;
    }
    Ok((
        directory,
        document_id.ok_or_else(|| failure("DOCUMENT_PARSE_FAILED"))?,
    ))
}

#[derive(Default)]
struct SlideEntry {
    persist_id: u32,
    slide_id: u32,
    outlines: Vec<String>,
}

fn text_characters(record: Record<'_>) -> Result<String, ContractError> {
    if record.version != 0 || record.instance != 0 {
        return Err(failure("DOCUMENT_PARSE_FAILED"));
    }
    let text = match record.kind {
        TEXT_CHARS => decode_utf16(record.payload)?,
        // TextBytesAtom stores the low byte of Unicode, not an ANSI code page.
        // TextBytesAtom 存储 Unicode 低字节；不按 ANSI 代码页改变源文字。
        TEXT_BYTES => record
            .payload
            .iter()
            .map(|byte| char::from(*byte))
            .collect(),
        _ => return Err(failure("DOCUMENT_PARSE_FAILED")),
    };
    if text.contains('\0') {
        return Err(failure("DOCUMENT_PARSE_FAILED"));
    }
    Ok(text.replace("\r\n", "\n").replace(['\r', '\u{000b}'], "\n"))
}

fn slide_entries(
    list: Record<'_>,
    budget: &mut ParseBudget<'_>,
    notes: bool,
) -> Result<Vec<SlideEntry>, ContractError> {
    if list.version != 15 || list.instance != if notes { 2 } else { 0 } {
        return Err(failure("DOCUMENT_PARSE_FAILED"));
    }
    let mut offset = 0;
    let mut entries: Vec<SlideEntry> = Vec::new();
    let mut ids = HashSet::new();
    let mut expected_text_count = 0;
    while let Some(record) = budget.next(list.payload, &mut offset)? {
        match record.kind {
            SLIDE_PERSIST => {
                expect_record(record, SLIDE_PERSIST, 0, Some(20))?;
                if entries
                    .last()
                    .is_some_and(|entry| !notes && entry.outlines.len() != expected_text_count)
                {
                    return Err(failure("DOCUMENT_PARSE_FAILED"));
                }
                let persist_id = u32_at(record.payload, 0)?;
                let slide_id = u32_at(record.payload, 12)?;
                if persist_id == 0
                    || slide_id == 0
                    || !ids.insert(slide_id)
                    || entries.len() >= 4096
                {
                    return Err(failure("DOCUMENT_STRUCTURE_LIMIT"));
                }
                expected_text_count = u32_at(record.payload, 8)? as usize;
                if (!notes && expected_text_count > 8) || (notes && expected_text_count != 0) {
                    return Err(failure("DOCUMENT_PARSE_FAILED"));
                }
                entries.push(SlideEntry {
                    persist_id,
                    slide_id,
                    outlines: Vec::new(),
                });
            }
            TEXT_HEADER if !notes => {
                if record.version != 0 || record.instance > 5 || record.payload.len() != 4 {
                    return Err(failure("DOCUMENT_PARSE_FAILED"));
                }
                let entry = entries
                    .last_mut()
                    .ok_or_else(|| failure("DOCUMENT_PARSE_FAILED"))?;
                if entry.outlines.len() >= 8 {
                    return Err(failure("DOCUMENT_STRUCTURE_LIMIT"));
                }
                entry.outlines.push(String::new());
            }
            TEXT_CHARS | TEXT_BYTES if !notes => {
                let text = entries
                    .last_mut()
                    .and_then(|entry| entry.outlines.last_mut())
                    .ok_or_else(|| failure("DOCUMENT_PARSE_FAILED"))?;
                if !text.is_empty() {
                    return Err(failure("DOCUMENT_PARSE_FAILED"));
                }
                *text = text_characters(record)?;
            }
            _ => {}
        }
    }
    if entries
        .last()
        .is_some_and(|entry| !notes && entry.outlines.len() != expected_text_count)
    {
        return Err(failure("DOCUMENT_PARSE_FAILED"));
    }
    Ok(entries)
}

fn resolve<'a>(
    stream: &'a [u8],
    directory: &HashMap<u32, usize>,
    persist_id: u32,
    kind: u16,
) -> Result<Record<'a>, ContractError> {
    let offset = *directory
        .get(&persist_id)
        .ok_or_else(|| failure("DOCUMENT_PARSE_FAILED"))?;
    let record = record_at(stream, offset)?;
    expect_record(record, kind, 15, None)?;
    Ok(record)
}

fn textbox(
    payload: &[u8],
    outlines: &[String],
    used_outlines: &mut HashSet<usize>,
    output: &mut DocumentText,
    budget: &mut ParseBudget<'_>,
) -> Result<(), ContractError> {
    let mut offset = 0;
    let mut has_text_header = false;
    while let Some(record) = budget.next(payload, &mut offset)? {
        match record.kind {
            OUTLINE_REF => {
                expect_record(record, OUTLINE_REF, 0, Some(4))?;
                let index = u32_at(record.payload, 0)? as usize;
                let text = outlines
                    .get(index)
                    .ok_or_else(|| failure("DOCUMENT_PARSE_FAILED"))?;
                used_outlines.insert(index);
                output.append(text)?;
                output.append("\n")?;
            }
            TEXT_HEADER => {
                expect_record(record, TEXT_HEADER, 0, Some(4))?;
                has_text_header = true;
            }
            TEXT_CHARS | TEXT_BYTES if has_text_header => {
                output.append(&text_characters(record)?)?;
                output.append("\n")?;
                has_text_header = false;
            }
            TEXT_CHARS | TEXT_BYTES => return Err(failure("DOCUMENT_PARSE_FAILED")),
            _ => {}
        }
    }
    Ok(())
}

fn is_deleted_shape(payload: &[u8], budget: &mut ParseBudget<'_>) -> Result<bool, ContractError> {
    let mut offset = 0;
    while let Some(record) = budget.next(payload, &mut offset)? {
        if record.kind == ART_SHAPE_FLAGS {
            if record.version != 2 || record.payload.len() != 8 {
                return Err(failure("DOCUMENT_PARSE_FAILED"));
            }
            return Ok(u32_at(record.payload, 4)? & 8 != 0);
        }
    }
    Err(failure("DOCUMENT_PARSE_FAILED"))
}

/// Deleted shapes can retain outline references; the fallback must not restore deleted text.
/// 删除形状可能仍保留大纲引用；备用大纲路径不得重新引入这些删除正文。
fn suppress_deleted_outlines(
    payload: &[u8],
    used_outlines: &mut HashSet<usize>,
    budget: &mut ParseBudget<'_>,
    depth: usize,
) -> Result<(), ContractError> {
    if depth >= 64 {
        return Err(failure("DOCUMENT_STRUCTURE_LIMIT"));
    }
    let mut offset = 0;
    while let Some(record) = budget.next(payload, &mut offset)? {
        match record.kind {
            ART_GROUP | ART_SHAPE | ART_TEXTBOX => {
                suppress_deleted_outlines(record.payload, used_outlines, budget, depth + 1)?;
            }
            OUTLINE_REF if record.payload.len() == 4 => {
                used_outlines.insert(u32_at(record.payload, 0)? as usize);
            }
            _ => {}
        }
    }
    Ok(())
}

/// Only drawing/group/shape records lead to textboxes; unknown blobs and OLE are not recursed.
/// 仅沿绘图、组和形状进入文本框；未知载荷、图片、宏与嵌入 OLE 不递归解释为正文。
fn drawing_text(
    record: Record<'_>,
    outlines: &[String],
    used_outlines: &mut HashSet<usize>,
    output: &mut DocumentText,
    budget: &mut ParseBudget<'_>,
    depth: usize,
) -> Result<(), ContractError> {
    if depth >= 64 {
        return Err(failure("DOCUMENT_STRUCTURE_LIMIT"));
    }
    if record.version != 15 {
        return Err(failure("DOCUMENT_PARSE_FAILED"));
    }
    if record.kind == ART_SHAPE && is_deleted_shape(record.payload, budget)? {
        suppress_deleted_outlines(record.payload, used_outlines, budget, depth)?;
        return Ok(());
    }
    if record.kind == ART_GROUP {
        let first = record_at(record.payload, 0)?;
        if first.kind != ART_SHAPE || first.version != 15 {
            return Err(failure("DOCUMENT_PARSE_FAILED"));
        }
        if is_deleted_shape(first.payload, budget)? {
            suppress_deleted_outlines(record.payload, used_outlines, budget, depth)?;
            return Ok(());
        }
    }
    let mut offset = 0;
    while let Some(child) = budget.next(record.payload, &mut offset)? {
        match child.kind {
            ART_DRAWING | ART_GROUP | ART_SHAPE => {
                drawing_text(child, outlines, used_outlines, output, budget, depth + 1)?
            }
            ART_TEXTBOX if record.kind == ART_SHAPE => {
                expect_record(child, ART_TEXTBOX, 15, None)?;
                textbox(child.payload, outlines, used_outlines, output, budget)?
            }
            _ => {}
        }
    }
    Ok(())
}

fn extract_slide(
    record: Record<'_>,
    entry: &SlideEntry,
    output: &mut DocumentText,
    budget: &mut ParseBudget<'_>,
    notes: bool,
) -> Result<u32, ContractError> {
    let mut offset = 0;
    let mut atom_found = false;
    let mut reference = 0;
    let mut used_outlines = HashSet::new();
    while let Some(child) = budget.next(record.payload, &mut offset)? {
        match child.kind {
            SLIDE_ATOM if !notes => {
                expect_record(child, SLIDE_ATOM, 2, Some(24))?;
                if atom_found {
                    return Err(failure("DOCUMENT_PARSE_FAILED"));
                }
                atom_found = true;
                reference = u32_at(child.payload, 16)?;
            }
            NOTES_ATOM if notes => {
                expect_record(child, NOTES_ATOM, 1, Some(8))?;
                if atom_found {
                    return Err(failure("DOCUMENT_PARSE_FAILED"));
                }
                atom_found = true;
                reference = u32_at(child.payload, 0)?;
            }
            DRAWING => drawing_text(
                child,
                &entry.outlines,
                &mut used_outlines,
                output,
                budget,
                0,
            )?,
            _ => {}
        }
    }
    if !atom_found {
        return Err(failure("DOCUMENT_PARSE_FAILED"));
    }
    for (index, text) in entry.outlines.iter().enumerate() {
        if !used_outlines.contains(&index) {
            output.append(text)?;
            output.append("\n")?;
        }
    }
    Ok(reference)
}

pub(super) fn decode(
    bytes: &[u8],
    limits: &Value,
    cancel: &Arc<AtomicBool>,
    digest: &str,
) -> Result<Value, ContractError> {
    cancelled(cancel)?;
    let mut compound = CompoundDocument::open(bytes, limits, cancel)?;
    let current = compound.read("/Current User", cancel)?;
    let stream = compound.read("/PowerPoint Document", cancel)?;
    let mut budget = ParseBudget {
        cancel,
        used_records: 0,
        maximum_records: limits["maximumDocumentRecords"]
            .as_u64()
            .unwrap_or(250_000)
            .clamp(1, 1_000_000) as usize,
    };
    let (directory, document_id) = persist_directory(&stream, &current, &mut budget)?;
    let document = resolve(&stream, &directory, document_id, DOCUMENT)?;
    let mut slide_list = None;
    let mut notes_list = None;
    let mut offset = 0;
    while let Some(record) = budget.next(document.payload, &mut offset)? {
        if record.kind == SLIDE_LIST {
            let target = match record.instance {
                0 => &mut slide_list,
                2 => &mut notes_list,
                _ => continue,
            };
            if target.replace(record).is_some() {
                return Err(failure("DOCUMENT_PARSE_FAILED"));
            }
        }
    }
    let slides = slide_entries(
        slide_list.ok_or_else(|| failure("DOCUMENT_TEXT_UNAVAILABLE"))?,
        &mut budget,
        false,
    )?;
    let notes = notes_list
        .map(|list| slide_entries(list, &mut budget, true))
        .transpose()?
        .unwrap_or_default();
    let notes_by_id: HashMap<_, _> = notes
        .into_iter()
        .map(|entry| (entry.slide_id, entry))
        .collect();
    let mut output = DocumentText::new(limits);
    let mut has_body_text = false;
    let mut used_persist_ids = HashSet::new();
    for (index, entry) in slides.iter().enumerate() {
        cancelled(cancel)?;
        if !used_persist_ids.insert(entry.persist_id) {
            return Err(failure("DOCUMENT_PARSE_FAILED"));
        }
        let slide = resolve(&stream, &directory, entry.persist_id, SLIDE)?;
        let name = format!("{} (slideId={})", index + 1, entry.slide_id);
        let start = output.start_part("slide", &name)?;
        let body_start = output.text.len();
        let notes_id = extract_slide(slide, entry, &mut output, &mut budget, false)?;
        has_body_text |= !output.text[body_start..].trim().is_empty();
        output.end_part("slide", &name, start);
        if notes_id != 0 {
            let note_entry = notes_by_id
                .get(&notes_id)
                .ok_or_else(|| failure("DOCUMENT_PARSE_FAILED"))?;
            if !used_persist_ids.insert(note_entry.persist_id) {
                return Err(failure("DOCUMENT_PARSE_FAILED"));
            }
            let note = resolve(&stream, &directory, note_entry.persist_id, NOTES)?;
            let start = output.start_part("notes", &name)?;
            let body_start = output.text.len();
            if extract_slide(note, note_entry, &mut output, &mut budget, true)? != entry.slide_id {
                return Err(failure("DOCUMENT_PARSE_FAILED"));
            }
            has_body_text |= !output.text[body_start..].trim().is_empty();
            output.end_part("notes", &name, start);
        }
    }
    if !has_body_text {
        return Err(failure("DOCUMENT_TEXT_UNAVAILABLE"));
    }
    output.finish(
        "ppt",
        "rust-msppt-text-v1",
        digest,
        &[
            "layout-and-formatting",
            "master-slides",
            "images-and-charts",
            "embedded-objects",
            "macros-and-external-links",
        ],
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::io::{Cursor, Write};

    fn fixture(name: &str) -> Vec<u8> {
        std::fs::read(
            std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("tests/fixtures/legacy-powerpoint")
                .join(name),
        )
        .unwrap()
    }

    fn extract(bytes: &[u8], limits: Value) -> Result<Value, ContractError> {
        decode(
            bytes,
            &limits,
            &Arc::new(AtomicBool::new(false)),
            &crate::retrieval::gateway_index::hash_bytes(bytes),
        )
    }

    #[test]
    fn independent_powerpoint_files_extract_bilingual_slides_notes_and_real_order() {
        let basic = fixture("basic_test_ppt_file.ppt");
        let result = extract(&basic, json!({})).unwrap();
        let text = result["text"].as_str().unwrap();
        for expected in [
            "This is a test title",
            "This is a test subtitle",
            "This is on page 1",
            "These are the notes for page 1",
            "This is the title on page 2",
            "This is page two",
            "It has several blocks of text",
            "None of them have formatting",
            "These are the notes on page two, again lacking formatting",
        ] {
            assert!(text.contains(expected), "missing {expected}: {text}");
        }
        assert!(text.find("This is a test title") < text.find("These are the notes for page 1"));
        assert!(
            text.find("These are the notes for page 1") < text.find("This is the title on page 2")
        );
        assert!(!text.contains("Click to edit"));
        assert_eq!(result["extraction"]["version"], "rust-msppt-text-v1");
        assert_eq!(
            result["extraction"]["rawContentHash"],
            crate::retrieval::gateway_index::hash_bytes(&basic)
        );
        assert_eq!(result["extraction"]["parts"].as_array().unwrap().len(), 4);
        crate::storage::retrieval::apply(
            "storage.retrieval.extraction",
            &json!({"value":result["extraction"],
            "textLength":text.encode_utf16().count()}),
        )
        .unwrap();
        assert_eq!(result, extract(&basic, json!({})).unwrap());

        let chinese = extract(&fixture("54880_chinese.ppt"), json!({})).unwrap();
        let chinese = chinese["text"].as_str().unwrap();
        for expected in ["Single byte", "Mix", "表", "ﾊﾝｶｸ"] {
            assert!(chinese.contains(expected), "{chinese}");
        }
        let textbox = extract(&fixture("with_textbox.ppt"), json!({})).unwrap();
        let textbox = textbox["text"].as_str().unwrap();
        for expected in [
            "Hello, World!!!",
            "I am just a poor boy",
            "This is Times New Roman",
            "Plain Text",
        ] {
            assert!(textbox.contains(expected), "{textbox}");
        }
        let reordered = extract(&fixture("incorrect_slide_order.ppt"), json!({})).unwrap();
        let reordered = reordered["text"].as_str().unwrap();
        assert!(reordered.find("First slide I added") < reordered.find("Third slide I added"));
        assert!(reordered.find("Third slide I added") < reordered.find("Second slide I added"));
    }

    fn record(kind: u16, version: u16, instance: u16, payload: &[u8]) -> Vec<u8> {
        let mut bytes = Vec::new();
        bytes.extend((version | instance << 4).to_le_bytes());
        bytes.extend(kind.to_le_bytes());
        bytes.extend((payload.len() as u32).to_le_bytes());
        bytes.extend(payload);
        bytes
    }

    fn compound(stream: &[u8], current: &[u8]) -> Vec<u8> {
        let mut compound = cfb::CompoundFile::create(Cursor::new(Vec::new())).unwrap();
        compound
            .create_stream("/PowerPoint Document")
            .unwrap()
            .write_all(stream)
            .unwrap();
        compound
            .create_stream("/Current User")
            .unwrap()
            .write_all(current)
            .unwrap();
        compound.into_inner().into_inner()
    }

    fn current(edit_offset: usize) -> Vec<u8> {
        let mut payload = Vec::new();
        payload.extend(20u32.to_le_bytes());
        payload.extend(0xe391c05fu32.to_le_bytes());
        payload.extend((edit_offset as u32).to_le_bytes());
        payload.extend(0u16.to_le_bytes());
        payload.extend(0x03f4u16.to_le_bytes());
        payload.extend([3, 0, 0, 0]);
        payload.extend(8u32.to_le_bytes());
        record(CURRENT_USER, 0, 0, &payload)
    }

    fn text(value: &str) -> Vec<u8> {
        let chars: Vec<u8> = value.encode_utf16().flat_map(u16::to_le_bytes).collect();
        let mut bytes = record(TEXT_HEADER, 0, 0, &1u32.to_le_bytes());
        bytes.extend(record(TEXT_CHARS, 0, 0, &chars));
        bytes
    }

    fn shape(payload: &[u8], deleted: bool) -> Vec<u8> {
        let mut flags = 100u32.to_le_bytes().to_vec();
        flags.extend((if deleted { 8u32 } else { 0 }).to_le_bytes());
        let mut children = record(ART_SHAPE_FLAGS, 2, 1, &flags);
        children.extend(record(ART_TEXTBOX, 15, 0, payload));
        record(ART_SHAPE, 15, 0, &children)
    }

    fn slide(notes_id: u32, body: &[u8], deleted: Option<&str>) -> Vec<u8> {
        let mut shapes = shape(body, false);
        if let Some(deleted) = deleted {
            shapes.extend(shape(&text(deleted), true));
        }
        slide_shapes(notes_id, &shapes)
    }

    fn slide_shapes(notes_id: u32, shapes: &[u8]) -> Vec<u8> {
        let mut atom = vec![0u8; 24];
        atom[12..16].copy_from_slice(&0x8000_0000u32.to_le_bytes());
        atom[16..20].copy_from_slice(&notes_id.to_le_bytes());
        let drawing = record(DRAWING, 15, 0, &record(ART_DRAWING, 15, 0, shapes));
        let mut children = record(SLIDE_ATOM, 2, 0, &atom);
        children.extend(drawing);
        record(SLIDE, 15, 0, &children)
    }

    fn note(slide_id: u32, body: &str) -> Vec<u8> {
        let mut atom = slide_id.to_le_bytes().to_vec();
        atom.extend(0u32.to_le_bytes());
        let mut children = record(NOTES_ATOM, 1, 0, &atom);
        children.extend(record(
            DRAWING,
            15,
            0,
            &record(ART_DRAWING, 15, 0, &shape(&text(body), false)),
        ));
        record(NOTES, 15, 0, &children)
    }

    fn list_entry(persist_id: u32, slide_id: u32, outline: Option<&str>) -> Vec<u8> {
        let mut payload = Vec::new();
        for value in [persist_id, 0, u32::from(outline.is_some()), slide_id, 0] {
            payload.extend(value.to_le_bytes());
        }
        let mut bytes = record(SLIDE_PERSIST, 0, 0, &payload);
        if let Some(outline) = outline {
            bytes.extend(text(outline));
        }
        bytes
    }

    fn document(slides: &[u8], notes: &[u8]) -> Vec<u8> {
        let mut children = record(SLIDE_LIST, 15, 0, slides);
        if !notes.is_empty() {
            children.extend(record(SLIDE_LIST, 15, 2, notes));
        }
        record(DOCUMENT, 15, 0, &children)
    }

    fn append(stream: &mut Vec<u8>, bytes: &[u8]) -> usize {
        let offset = stream.len();
        stream.extend(bytes);
        offset
    }

    fn save(stream: &mut Vec<u8>, previous: usize, mappings: &[(u32, usize)]) -> usize {
        let mut directory = Vec::new();
        for (id, offset) in mappings {
            directory.extend((id | 1 << 20).to_le_bytes());
            directory.extend((*offset as u32).to_le_bytes());
        }
        let directory_offset = append(stream, &record(PERSIST_DIRECTORY, 0, 0, &directory));
        let mut edit = vec![0u8; 28];
        edit[7] = 3;
        edit[8..12].copy_from_slice(&(previous as u32).to_le_bytes());
        edit[12..16].copy_from_slice(&(directory_offset as u32).to_le_bytes());
        edit[16..20].copy_from_slice(&1u32.to_le_bytes());
        edit[20..24].copy_from_slice(&100u32.to_le_bytes());
        append(stream, &record(USER_EDIT, 0, 0, &edit))
    }

    /// Appended saves deliberately retain old and deleted text in the same real CFB file.
    /// 增量保存夹具故意在同一真实 CFB 内保留旧正文与删除正文，以验证只读取活动版本。
    fn edited_stream() -> (Vec<u8>, Vec<u8>) {
        let mut stream = Vec::new();
        let mut old_list = list_entry(2, 256, None);
        old_list.extend(list_entry(3, 257, None));
        old_list.extend(list_entry(4, 258, None));
        let old_document = append(&mut stream, &document(&old_list, &list_entry(5, 500, None)));
        let old_a = append(&mut stream, &slide(500, &text("STALE_OLD_SLIDE"), None));
        let b = append(&mut stream, &slide(0, &text("Second source slide"), None));
        let removed = append(
            &mut stream,
            &slide(0, &text("DELETED_SLIDE_MUST_NOT_APPEAR"), None),
        );
        let old_notes = append(&mut stream, &note(256, "STALE_OLD_NOTES"));
        let previous = save(
            &mut stream,
            0,
            &[
                (1, old_document),
                (2, old_a),
                (3, b),
                (4, removed),
                (5, old_notes),
            ],
        );
        let mut active_list = list_entry(3, 257, None);
        active_list.extend(list_entry(2, 256, Some("中文标题😀 English")));
        let active_document = append(
            &mut stream,
            &document(&active_list, &list_entry(5, 500, None)),
        );
        let mut outline_and_body = shape(&record(OUTLINE_REF, 0, 0, &0u32.to_le_bytes()), false);
        outline_and_body.extend(shape(&text("当前正文量子检索"), false));
        outline_and_body.extend(shape(&text("DELETED_SHAPE_MUST_NOT_APPEAR"), true));
        let active_a = append(&mut stream, &slide_shapes(500, &outline_and_body));
        let active_notes = append(&mut stream, &note(256, "最新备注 bilingual notes"));
        let latest = save(
            &mut stream,
            previous,
            &[(1, active_document), (2, active_a), (5, active_notes)],
        );
        (stream, current(latest))
    }

    #[test]
    fn latest_persist_objects_exclude_deleted_slides_shapes_and_old_versions() {
        let (stream, current) = edited_stream();
        let bytes = compound(&stream, &current);
        let result = extract(&bytes, json!({})).unwrap();
        let text = result["text"].as_str().unwrap();
        assert!(text.find("Second source slide") < text.find("中文标题😀 English"));
        assert!(text.contains("当前正文量子检索"));
        assert!(text.contains("最新备注 bilingual notes"));
        for removed in [
            "STALE_OLD_SLIDE",
            "STALE_OLD_NOTES",
            "DELETED_SLIDE_MUST_NOT_APPEAR",
            "DELETED_SHAPE_MUST_NOT_APPEAR",
        ] {
            assert!(!text.contains(removed), "{text}");
        }
        assert_eq!(text.matches("中文标题😀 English").count(), 1);
        assert_eq!(result["extraction"]["parts"].as_array().unwrap().len(), 3);
        assert_eq!(
            result["extraction"]["parts"][2]["endOffset"],
            text.encode_utf16().count()
        );
    }

    #[test]
    fn malformed_encrypted_and_out_of_budget_binary_presentations_are_typed_failures() {
        let (stream, mut user) = edited_stream();
        user[12..16].copy_from_slice(&0xf3d1c4dfu32.to_le_bytes());
        assert_eq!(
            extract(&compound(&stream, &user), json!({}))
                .unwrap_err()
                .code(),
            "DOCUMENT_ENCRYPTED"
        );
        let (_, user) = edited_stream();
        let mut corrupted = stream.clone();
        let edit_offset = u32_at(&user, 16).unwrap() as usize;
        corrupted[edit_offset + 16..edit_offset + 20]
            .copy_from_slice(&(edit_offset as u32).to_le_bytes());
        assert_eq!(
            extract(&compound(&corrupted, &user), json!({}))
                .unwrap_err()
                .code(),
            "DOCUMENT_PARSE_FAILED"
        );
        let bytes = compound(&stream, &user);
        assert_eq!(
            extract(&bytes, json!({"maximumDocumentOutputBytes":16}))
                .unwrap_err()
                .code(),
            "DOCUMENT_OUTPUT_LIMIT"
        );
        assert_eq!(
            extract(&bytes, json!({"maximumDocumentRecords":1}))
                .unwrap_err()
                .code(),
            "DOCUMENT_STRUCTURE_LIMIT"
        );
        assert_eq!(
            extract(&bytes, json!({"maximumArchiveEntryBytes":16}))
                .unwrap_err()
                .code(),
            "DOCUMENT_ARCHIVE_LIMIT"
        );
        assert_eq!(
            decode(&bytes, &json!({}), &Arc::new(AtomicBool::new(true)), "hash")
                .unwrap_err()
                .code(),
            "ABORT_ERR"
        );
        assert_eq!(
            extract(&bytes[..1024], json!({})).unwrap_err().code(),
            "DOCUMENT_PARSE_FAILED"
        );
        let invalid_utf16 = record(TEXT_CHARS, 0, 0, &0xd800u16.to_le_bytes());
        assert_eq!(
            text_characters(record_at(&invalid_utf16, 0).unwrap())
                .unwrap_err()
                .code(),
            "DOCUMENT_TEXT_ENCODING_UNSUPPORTED"
        );
        let latin = record(TEXT_BYTES, 0, 0, &[0x80, 0xa3, b'A']);
        assert_eq!(
            text_characters(record_at(&latin, 0).unwrap()).unwrap(),
            "\u{0080}£A"
        );
    }
}

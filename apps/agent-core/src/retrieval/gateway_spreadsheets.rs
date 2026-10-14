//! Workbook values and formulas remain source text, never executable spreadsheet instructions.
//! 工作簿数值与公式只作为来源文字，不执行公式、宏或外部数据链接。

use super::gateway_office_documents::{DocumentPackage, DocumentText, cancelled, failure};
use crate::storage::ContractError;
use calamine::{Data, DataRef, Reader, Xls, Xlsb, Xlsx};
use serde_json::Value;
use std::{
    io::{Cursor, Read},
    sync::{Arc, atomic::AtomicBool},
};

fn cell_address(row: u32, column: u32) -> Result<String, ContractError> {
    if row >= 1048576 || column >= 16384 {
        return Err(failure("DOCUMENT_STRUCTURE_LIMIT"));
    }
    let mut number = column + 1;
    let mut letters = Vec::new();
    while number > 0 {
        letters.push((b'A' + ((number - 1) % 26) as u8) as char);
        number = (number - 1) / 26;
    }
    Ok(format!(
        "{}{}",
        letters.into_iter().rev().collect::<String>(),
        row + 1
    ))
}

fn cell_text(value: &DataRef<'_>) -> String {
    match value {
        DataRef::DateTime(value) if value.is_datetime() => value
            .as_datetime()
            .map(|date| date.format("%Y-%m-%dT%H:%M:%S").to_string())
            .unwrap_or_else(|| value.to_string()),
        DataRef::Int(value) => value.to_string(),
        DataRef::Float(value) => value.to_string(),
        DataRef::String(value) | DataRef::DateTimeIso(value) | DataRef::DurationIso(value) => {
            value.clone()
        }
        DataRef::SharedString(value) => (*value).to_string(),
        DataRef::Bool(value) => value.to_string(),
        DataRef::DateTime(value) => value.to_string(),
        DataRef::Error(value) => value.to_string(),
        DataRef::Empty => String::new(),
    }
}

fn append_cell(
    output: &mut DocumentText,
    row: u32,
    column: u32,
    value: &str,
    formula: bool,
) -> Result<(), ContractError> {
    if !value.is_empty() {
        output.append(&format!(
            "{}\t{}{}\n",
            cell_address(row, column)?,
            if formula { "formula=" } else { "value=" },
            value
        ))?;
    }
    Ok(())
}

/// Legacy BIFF dimensions are bounded before Calamine reserves a dense range.
/// 在 Calamine 根据旧 BIFF 维度预留稠密数组之前检查额度，避免小文件声明巨量空格网格。
fn preflight_xls(
    bytes: &[u8],
    limits: &Value,
    cancel: &Arc<AtomicBool>,
) -> Result<(), ContractError> {
    let mut compound = cfb::CompoundFile::open(Cursor::new(bytes))
        .map_err(|_| failure("DOCUMENT_PARSE_FAILED"))?;
    let name = if compound.is_stream("/Workbook") {
        "/Workbook"
    } else {
        "/Book"
    };
    let maximum = limits["maximumArchiveEntryBytes"]
        .as_u64()
        .unwrap_or(64 * 1024 * 1024);
    let mut stream = compound
        .open_stream(name)
        .map_err(|_| failure("DOCUMENT_PARSE_FAILED"))?;
    if stream.len() > maximum {
        return Err(failure("DOCUMENT_ARCHIVE_LIMIT"));
    }
    let mut workbook = Vec::new();
    stream
        .read_to_end(&mut workbook)
        .map_err(|_| failure("DOCUMENT_PARSE_FAILED"))?;
    let maximum_cells = limits["maximumSpreadsheetCells"]
        .as_u64()
        .unwrap_or(2_000_000)
        .clamp(1, 8_000_000);
    let mut position = 0usize;
    let mut minimum_row = u64::MAX;
    let mut maximum_row = 0u64;
    let mut minimum_column = u64::MAX;
    let mut maximum_column = 0u64;
    while position < workbook.len() {
        cancelled(cancel)?;
        let header = workbook
            .get(position..position + 4)
            .ok_or_else(|| failure("DOCUMENT_PARSE_FAILED"))?;
        let kind = u16::from_le_bytes([header[0], header[1]]);
        let length = u16::from_le_bytes([header[2], header[3]]) as usize;
        let body = workbook
            .get(position + 4..position + 4 + length)
            .ok_or_else(|| failure("DOCUMENT_PARSE_FAILED"))?;
        if kind == 0x002f {
            return Err(failure("DOCUMENT_ENCRYPTED"));
        }
        if kind == 0x0200 {
            let (first_row, end_row, first_column, end_column) = match length {
                10 => (
                    u16_at(body, 0)? as u64,
                    u16_at(body, 2)? as u64,
                    u16_at(body, 4)? as u64,
                    u16_at(body, 6)? as u64,
                ),
                14 => (
                    u32_at(body, 0)? as u64,
                    u32_at(body, 4)? as u64,
                    u16_at(body, 8)? as u64,
                    u16_at(body, 10)? as u64,
                ),
                _ => return Err(failure("DOCUMENT_PARSE_FAILED")),
            };
            if end_row < first_row
                || end_column < first_column
                || end_row > 65536
                || end_column > 256
                || (end_row - first_row).saturating_mul(end_column - first_column) > maximum_cells
            {
                return Err(failure("DOCUMENT_STRUCTURE_LIMIT"));
            }
        }
        if [0x0203, 0x0204, 0x0205, 0x027e, 0x00fd, 0x00bd, 0x0006].contains(&kind) {
            let row = u16_at(body, 0)? as u64;
            let column = u16_at(body, 2)? as u64;
            minimum_row = minimum_row.min(row);
            maximum_row = maximum_row.max(row);
            minimum_column = minimum_column.min(column);
            maximum_column = maximum_column.max(column);
            if (maximum_row - minimum_row + 1).saturating_mul(maximum_column - minimum_column + 1)
                > maximum_cells
            {
                return Err(failure("DOCUMENT_STRUCTURE_LIMIT"));
            }
        }
        if kind == 0x000a {
            minimum_row = u64::MAX;
            maximum_row = 0;
            minimum_column = u64::MAX;
            maximum_column = 0;
        }
        position += 4 + length;
    }
    Ok(())
}

fn u16_at(bytes: &[u8], position: usize) -> Result<u16, ContractError> {
    let data = bytes
        .get(position..position + 2)
        .ok_or_else(|| failure("DOCUMENT_PARSE_FAILED"))?;
    Ok(u16::from_le_bytes([data[0], data[1]]))
}
fn u32_at(bytes: &[u8], position: usize) -> Result<u32, ContractError> {
    let data = bytes
        .get(position..position + 4)
        .ok_or_else(|| failure("DOCUMENT_PARSE_FAILED"))?;
    Ok(u32::from_le_bytes([data[0], data[1], data[2], data[3]]))
}

/// Binary XLSB record lengths and shared-string indexes are checked before the library borrows them.
/// 调用库读取 XLSB 前核验二进制记录长度及共享字符串索引，避免畸形记录越界。
fn preflight_xlsb(
    package: &mut DocumentPackage<'_>,
    cancel: &Arc<AtomicBool>,
) -> Result<(), ContractError> {
    let names: Vec<_> = package
        .names
        .iter()
        .filter(|name| name.starts_with("xl/") && name.ends_with(".bin"))
        .cloned()
        .collect();
    let mut shared_strings = 0u32;
    let mut maximum_reference = None;
    for name in names {
        let bytes = package.read(&name, cancel)?;
        let mut position = 0usize;
        while position < bytes.len() {
            cancelled(cancel)?;
            let first = *bytes
                .get(position)
                .ok_or_else(|| failure("DOCUMENT_PARSE_FAILED"))?;
            position += 1;
            let kind = if first & 0x80 == 0 {
                first as u16
            } else {
                let second = *bytes
                    .get(position)
                    .ok_or_else(|| failure("DOCUMENT_PARSE_FAILED"))?;
                position += 1;
                (first & 0x7f) as u16 | ((second & 0x7f) as u16) << 7
            };
            let mut length = 0usize;
            let mut terminated = false;
            for shift in (0..28).step_by(7) {
                let next = *bytes
                    .get(position)
                    .ok_or_else(|| failure("DOCUMENT_PARSE_FAILED"))?;
                position += 1;
                length |= ((next & 0x7f) as usize) << shift;
                if next & 0x80 == 0 {
                    terminated = true;
                    break;
                }
            }
            if !terminated {
                return Err(failure("DOCUMENT_PARSE_FAILED"));
            }
            let body = bytes
                .get(position..position.saturating_add(length))
                .ok_or_else(|| failure("DOCUMENT_PARSE_FAILED"))?;
            let minimum = match kind {
                0x0000 => 4,
                0x0002 | 0x0007 => 12,
                0x0003 | 0x0004 | 0x000a | 0x000b => 9,
                0x0005 | 0x0009 | 0x0094 => 16,
                0x0006 | 0x0008 => 12,
                0x009f => 8,
                0x0013 => 5,
                0x0099 => 1,
                0x009c => 16,
                _ => 0,
            };
            if body.len() < minimum {
                return Err(failure("DOCUMENT_PARSE_FAILED"));
            }
            if name == "xl/sharedStrings.bin" && kind == 0x0013 {
                shared_strings += 1;
            }
            if name.starts_with("xl/worksheets/") && kind == 0x0007 {
                let reference = u32_at(body, 8)?;
                maximum_reference = Some(maximum_reference.unwrap_or(0).max(reference));
            }
            if name.starts_with("xl/worksheets/")
                && [0x0008, 0x0009, 0x000a, 0x000b].contains(&kind)
            {
                let formula_offset = match kind {
                    0x0008 => 14usize
                        .checked_add((u32_at(body, 8)? as usize).saturating_mul(2))
                        .ok_or_else(|| failure("DOCUMENT_PARSE_FAILED"))?,
                    0x0009 => 18,
                    _ => 11,
                };
                let formula_length = u32_at(body, formula_offset)? as usize;
                if body
                    .get(
                        formula_offset + 4
                            ..formula_offset
                                .saturating_add(4)
                                .saturating_add(formula_length),
                    )
                    .is_none()
                {
                    return Err(failure("DOCUMENT_PARSE_FAILED"));
                }
            }
            position += length;
        }
    }
    if maximum_reference.is_some_and(|reference| reference >= shared_strings) {
        return Err(failure("DOCUMENT_PARSE_FAILED"));
    }
    Ok(())
}

pub(crate) fn decode(
    format: &str,
    bytes: &[u8],
    limits: &Value,
    cancel: &Arc<AtomicBool>,
    digest: &str,
) -> Result<Value, ContractError> {
    cancelled(cancel)?;
    let mut output = DocumentText::new(limits);
    output.append("[Workbook: stored cell values and formulas; formulas are not evaluated]\n")?;
    match format {
        "xlsx" | "xlsm" | "xlam" | "xltx" | "xltm" => {
            let _package = DocumentPackage::open(bytes, limits, cancel)?;
            let mut workbook =
                Xlsx::new(Cursor::new(bytes)).map_err(|_| failure("DOCUMENT_PARSE_FAILED"))?;
            let sheets = workbook.sheet_names().to_vec();
            for name in sheets {
                cancelled(cancel)?;
                let start = output.start_part("sheet", &name)?;
                let mut reader = workbook
                    .worksheet_cells_reader(&name)
                    .map_err(|_| failure("DOCUMENT_PARSE_FAILED"))?;
                while let Some(cell) = reader
                    .next_cell()
                    .map_err(|_| failure("DOCUMENT_PARSE_FAILED"))?
                {
                    cancelled(cancel)?;
                    let (row, column) = cell.get_position();
                    append_cell(
                        &mut output,
                        row,
                        column,
                        &cell_text(cell.get_value()),
                        false,
                    )?;
                }
                drop(reader);
                let mut reader = workbook
                    .worksheet_cells_reader(&name)
                    .map_err(|_| failure("DOCUMENT_PARSE_FAILED"))?;
                while let Some(cell) = reader
                    .next_formula()
                    .map_err(|_| failure("DOCUMENT_PARSE_FAILED"))?
                {
                    cancelled(cancel)?;
                    let (row, column) = cell.get_position();
                    append_cell(&mut output, row, column, cell.get_value(), true)?;
                }
                output.end_part("sheet", &name, start);
            }
        }
        "xlsb" => {
            let mut package = DocumentPackage::open(bytes, limits, cancel)?;
            preflight_xlsb(&mut package, cancel)?;
            let mut workbook =
                Xlsb::new(Cursor::new(bytes)).map_err(|_| failure("DOCUMENT_PARSE_FAILED"))?;
            for name in workbook.sheet_names().to_vec() {
                let start = output.start_part("sheet", &name)?;
                let mut reader = workbook
                    .worksheet_cells_reader(&name)
                    .map_err(|_| failure("DOCUMENT_PARSE_FAILED"))?;
                while let Some(cell) = reader
                    .next_cell()
                    .map_err(|_| failure("DOCUMENT_PARSE_FAILED"))?
                {
                    cancelled(cancel)?;
                    let (row, column) = cell.get_position();
                    append_cell(
                        &mut output,
                        row,
                        column,
                        &cell_text(cell.get_value()),
                        false,
                    )?;
                }
                drop(reader);
                let mut reader = workbook
                    .worksheet_cells_reader(&name)
                    .map_err(|_| failure("DOCUMENT_PARSE_FAILED"))?;
                while let Some(cell) = reader
                    .next_formula()
                    .map_err(|_| failure("DOCUMENT_PARSE_FAILED"))?
                {
                    cancelled(cancel)?;
                    let (row, column) = cell.get_position();
                    append_cell(&mut output, row, column, cell.get_value(), true)?;
                }
                output.end_part("sheet", &name, start);
            }
        }
        "xls" => {
            preflight_xls(bytes, limits, cancel)?;
            let mut workbook =
                Xls::new(Cursor::new(bytes)).map_err(|_| failure("DOCUMENT_PARSE_FAILED"))?;
            for name in workbook.sheet_names().to_vec() {
                cancelled(cancel)?;
                let start = output.start_part("sheet", &name)?;
                let values = workbook
                    .worksheet_range(&name)
                    .map_err(|_| failure("DOCUMENT_PARSE_FAILED"))?;
                let origin = values.start().unwrap_or((0, 0));
                for (row, column, value) in values.used_cells() {
                    cancelled(cancel)?;
                    let text = match value {
                        Data::DateTime(date) if date.is_datetime() => date
                            .as_datetime()
                            .map(|value| value.format("%Y-%m-%dT%H:%M:%S").to_string())
                            .unwrap_or_else(|| date.to_string()),
                        value => value.to_string(),
                    };
                    append_cell(
                        &mut output,
                        origin.0 + row as u32,
                        origin.1 + column as u32,
                        &text,
                        false,
                    )?;
                }
                let formulas = workbook
                    .worksheet_formula(&name)
                    .map_err(|_| failure("DOCUMENT_PARSE_FAILED"))?;
                let origin = formulas.start().unwrap_or((0, 0));
                for (row, column, formula) in formulas.used_cells() {
                    append_cell(
                        &mut output,
                        origin.0 + row as u32,
                        origin.1 + column as u32,
                        formula,
                        true,
                    )?;
                }
                output.end_part("sheet", &name, start);
            }
        }
        "ods" => {
            // The shared container path also rejects encrypted manifests before extracting sheet XML.
            // 共用容器路径先拒绝加密 manifest，再提取表格 XML，不能绕过文档加密校验。
            return super::gateway_office_documents::open_document(
                "ods", bytes, limits, cancel, digest,
            );
        }
        _ => return Err(failure("DOCUMENT_FORMAT_UNSUPPORTED")),
    }
    output.finish(
        format,
        "rust-office-text-v1",
        digest,
        &[
            "embedded-images",
            "macros",
            "external-links",
            "display-number-formatting",
        ],
    )
}

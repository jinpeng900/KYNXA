//! 表格导入保留单元格类型，类别和缺失值编码留给训练器仅用训练集拟合。
use super::invalid;
use crate::gateway_contract::GatewayError;
use calamine::{Data, DataRef, Reader, Xls, Xlsx};
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, HashSet},
    io::Cursor,
    sync::{Arc, atomic::AtomicBool},
};

pub(super) fn delimited(text: &str, delimiter: char) -> Result<Vec<Vec<String>>, GatewayError> {
    let mut rows = Vec::new();
    let mut row = Vec::new();
    let mut cell = String::new();
    let mut quoted = false;
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '"' {
            if quoted && chars.peek() == Some(&'"') {
                cell.push('"');
                chars.next();
            } else if quoted || cell.is_empty() {
                quoted = !quoted;
            } else {
                return Err(invalid("CSV 引号位置不正确。"));
            }
        } else if c == delimiter && !quoted {
            row.push(std::mem::take(&mut cell));
        } else if c == '\n' && !quoted {
            row.push(std::mem::take(&mut cell));
            if row.iter().any(|c| !c.trim().is_empty()) {
                rows.push(std::mem::take(&mut row));
            } else {
                row.clear();
            }
        } else if c != '\r' || quoted {
            cell.push(c);
        }
    }
    if quoted {
        return Err(invalid("CSV 有未闭合的引号。"));
    }
    if !cell.is_empty() || !row.is_empty() {
        row.push(cell);
        rows.push(row);
    }
    Ok(rows)
}
pub(super) fn table_samples(
    text: &str,
    delimiter: char,
    input: &Value,
    source: &Value,
) -> Result<(Vec<Value>, Vec<String>), GatewayError> {
    let records = delimited(text, delimiter)?;
    let records = records
        .into_iter()
        .enumerate()
        .map(|(row, cells)| {
            cells
                .into_iter()
                .map(|s| {
                    if row == 0 && input["hasHeader"] != false {
                        json!(s)
                    } else if s.trim().is_empty() {
                        Value::Null
                    } else {
                        s.trim()
                            .parse::<f64>()
                            .ok()
                            .filter(|x| x.is_finite())
                            .map(|x| json!(x))
                            .unwrap_or_else(|| json!(s))
                    }
                })
                .collect()
        })
        .collect();
    record_samples(records, input, source)
}
fn record_samples(
    mut records: Vec<Vec<Value>>,
    input: &Value,
    source: &Value,
) -> Result<(Vec<Value>, Vec<String>), GatewayError> {
    if records.is_empty() {
        return Err(invalid("表格没有数据。"));
    }
    let header = if input["hasHeader"] == false {
        (0..records[0].len())
            .map(|i| format!("column_{}", i + 1))
            .collect::<Vec<_>>()
    } else {
        records
            .remove(0)
            .iter()
            .map(|v| {
                v.as_str().map(str::to_owned).unwrap_or_else(|| {
                    if v.is_null() {
                        String::new()
                    } else {
                        v.to_string()
                    }
                })
            })
            .map(|name| name.trim().to_owned())
            .collect()
    };
    if header.is_empty()
        || header.iter().any(String::is_empty)
        || header.iter().collect::<HashSet<_>>().len() != header.len()
    {
        return Err(invalid("表格列名为空或重复。"));
    }
    let column = |value: &Value| -> Result<usize, GatewayError> {
        value
            .as_u64()
            .map(|i| i as usize)
            .or_else(|| {
                value
                    .as_str()
                    .and_then(|s| header.iter().position(|h| h == s))
            })
            .filter(|i| *i < header.len())
            .ok_or_else(|| invalid("所选特征列或标签列不存在。"))
    };
    let label = if input.get("targetColumn").is_some() {
        Some(column(&input["targetColumn"])?)
    } else if input["task"] == "clustering" {
        None
    } else {
        Some(header.len() - 1)
    };
    let features = if let Some(chosen) = input["featureColumns"].as_array() {
        chosen.iter().map(column).collect::<Result<Vec<_>, _>>()?
    } else {
        (0..header.len()).filter(|i| Some(*i) != label).collect()
    };
    if features.is_empty()
        || features.iter().any(|i| Some(*i) == label)
        || features.iter().collect::<HashSet<_>>().len() != features.len()
    {
        return Err(invalid("请选择标签以外的非空、不重复特征列。"));
    }
    if records.len() > 200_000 {
        return Err(invalid("表格超过 200000 行，请分批导入。"));
    }
    let mut samples = Vec::new();
    for (index, record) in records.into_iter().enumerate() {
        if record.len() != header.len() {
            return Err(invalid("表格行的列数与表头不一致。"));
        }
        let mut sample = json!({"features":features.iter().map(|i|record[*i].clone()).collect::<Vec<_>>(),"source":source,"sourceRow":index+if input["hasHeader"]==false{1}else{2}});
        if let Some(i) = label {
            if record[i].is_null() || record[i].as_str().is_some_and(|s| s.trim().is_empty()) {
                return Err(invalid("表格存在缺少标签的行，请补齐或移除。"));
            }
            sample["label"] = record[i].clone();
            sample["targetName"] = json!(header[i]);
        }
        samples.push(sample);
    }
    Ok((
        samples,
        features.iter().map(|i| header[*i].clone()).collect(),
    ))
}
fn reference_cell(cell: &DataRef<'_>) -> Result<Value, GatewayError> {
    Ok(match cell {
        DataRef::Empty => Value::Null,
        DataRef::Int(n) => json!(n),
        DataRef::Float(n) => {
            if !n.is_finite() {
                return Err(invalid("工作簿含非有限数值。"));
            }
            json!(n)
        }
        DataRef::Bool(value) => json!(if *value { 1 } else { 0 }),
        DataRef::String(s) | DataRef::DateTimeIso(s) | DataRef::DurationIso(s) => json!(s),
        DataRef::SharedString(s) => json!(s),
        DataRef::DateTime(date) => json!(
            date.as_datetime()
                .map(|d| d.format("%Y-%m-%dT%H:%M:%S").to_string())
                .unwrap_or_else(|| date.to_string())
        ),
        DataRef::Error(_) => {
            return Err(invalid(
                "工作簿包含错误单元格，请先修正；导入不会执行或重新计算公式。",
            ));
        }
    })
}
fn owned_cell(cell: &Data) -> Result<Value, GatewayError> {
    Ok(match cell {
        Data::Empty => Value::Null,
        Data::Int(n) => json!(n),
        Data::Float(n) => {
            if !n.is_finite() {
                return Err(invalid("工作簿含非有限数值。"));
            }
            json!(n)
        }
        Data::Bool(value) => json!(if *value { 1 } else { 0 }),
        Data::String(s) | Data::DateTimeIso(s) | Data::DurationIso(s) => json!(s),
        Data::DateTime(date) => json!(
            date.as_datetime()
                .map(|d| d.format("%Y-%m-%dT%H:%M:%S").to_string())
                .unwrap_or_else(|| date.to_string())
        ),
        Data::Error(_) => return Err(invalid("工作簿包含错误单元格，请先修正。")),
    })
}
pub(super) fn spreadsheet_samples(
    bytes: &[u8],
    extension: &str,
    input: &Value,
    source: &Value,
    cancel: &Arc<AtomicBool>,
) -> Result<(Vec<Value>, Vec<String>), GatewayError> {
    use crate::retrieval::{
        gateway_office_documents::{DocumentPackage, cancelled},
        gateway_spreadsheets,
    };
    let limits = json!({"maximumArchiveExpandedBytes":64*1024*1024,"maximumArchiveEntryBytes":32*1024*1024,"maximumSpreadsheetCells":2_000_000});
    let mut cells: BTreeMap<u32, BTreeMap<u32, Value>> = BTreeMap::new();
    let sheet;
    if extension == "xls" {
        gateway_spreadsheets::preflight_xls(bytes, &limits, cancel)?;
        let mut book = Xls::new(Cursor::new(bytes)).map_err(|_| invalid("XLS 工作簿无法读取。"))?;
        sheet = input["sheetName"]
            .as_str()
            .filter(|s| !s.is_empty())
            .map(str::to_owned)
            .or_else(|| book.sheet_names().first().cloned())
            .ok_or_else(|| invalid("工作簿没有工作表。"))?;
        let range = book
            .worksheet_range(&sheet)
            .map_err(|_| invalid("所选工作表不存在或无法读取。"))?;
        for (r, c, cell) in range.used_cells() {
            cancelled(cancel)?;
            cells
                .entry(r as u32)
                .or_default()
                .insert(c as u32, owned_cell(cell)?);
        }
    } else {
        let _package = DocumentPackage::open(bytes, &limits, cancel)?;
        let mut book =
            Xlsx::new(Cursor::new(bytes)).map_err(|_| invalid("XLSX 工作簿无法读取。"))?;
        sheet = input["sheetName"]
            .as_str()
            .filter(|s| !s.is_empty())
            .map(str::to_owned)
            .or_else(|| book.sheet_names().first().cloned())
            .ok_or_else(|| invalid("工作簿没有工作表。"))?;
        let mut reader = book
            .worksheet_cells_reader(&sheet)
            .map_err(|_| invalid("所选工作表不存在或无法读取。"))?;
        let mut count = 0;
        while let Some(cell) = reader
            .next_cell()
            .map_err(|_| invalid("工作表单元格无法读取。"))?
        {
            cancelled(cancel)?;
            let (row, col) = cell.get_position();
            count += 1;
            if count > 2_000_000 || cells.len() > 200_001 {
                return Err(invalid("工作表超出训练数据集容量，请选择范围更小的资料。"));
            }
            let value = reference_cell(cell.get_value())?;
            if !value.is_null() {
                cells.entry(row).or_default().insert(col, value);
            }
        }
    }
    let first = cells
        .values()
        .filter_map(|row| row.keys().next().copied())
        .min()
        .ok_or_else(|| invalid("所选工作表为空。"))?;
    let last = cells
        .values()
        .filter_map(|row| row.keys().last().copied())
        .max()
        .unwrap();
    if last - first >= 4096 {
        return Err(invalid("工作表特征列过多，请选择较小的表格。"));
    }
    let row_numbers = cells.keys().copied().collect::<Vec<_>>();
    let records = cells
        .into_values()
        .map(|row| {
            (first..=last)
                .map(|column| row.get(&column).cloned().unwrap_or(Value::Null))
                .collect()
        })
        .collect();
    let mut source = source.clone();
    source["sheetName"] = json!(sheet);
    source["dateEncoding"] = json!("ISO-8601-text");
    source["formulasEvaluated"] = json!(false);
    let (mut samples, names) = record_samples(records, input, &source)?;
    for (sample, row) in samples.iter_mut().zip(
        row_numbers
            .into_iter()
            .skip(if input["hasHeader"] == false { 0 } else { 1 }),
    ) {
        sample["sourceRow"] = json!(row + 1);
    }
    Ok((samples, names))
}

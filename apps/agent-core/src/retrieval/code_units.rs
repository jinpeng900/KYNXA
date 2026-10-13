use std::collections::BinaryHeap;
use std::time::{Duration, Instant};

use serde_json::{Value, json};

use super::chunking;

/// A sweep assigns each AST span to its deepest declaration without treating syntactic ownership as resolved references.
/// 扫描线把 AST 区间分配给最深声明；语法归属不等于已经解析的引用或真实调用关系。
pub fn select(args: &Value) -> Result<Value, String> {
    let text = args["text"]
        .as_str()
        .ok_or("INVALID_CODE_SOURCE")?
        .encode_utf16()
        .collect::<Vec<_>>();
    let declarations = args["declarations"]
        .as_array()
        .filter(|items| items.len() <= 10000)
        .ok_or("INVALID_RETRIEVAL_STRUCTURE")?;
    let mut events = Vec::with_capacity(declarations.len() * 2);
    for (owner, declaration) in declarations.iter().enumerate() {
        let unit = &declaration["unit"];
        let start = unit["startOffset"]
            .as_u64()
            .filter(|offset| *offset <= text.len() as u64)
            .ok_or("INVALID_RETRIEVAL_STRUCTURE")? as usize;
        let end = unit["endOffset"]
            .as_u64()
            .filter(|offset| *offset <= text.len() as u64)
            .ok_or("INVALID_RETRIEVAL_STRUCTURE")? as usize;
        if end <= start || chunking::split_pair(&text, start) || chunking::split_pair(&text, end) {
            return Err("INVALID_RETRIEVAL_STRUCTURE".into());
        }
        events.push((start, true, owner));
        events.push((end, false, owner));
    }
    events.sort_by_key(|(offset, active, _)| (*offset, *active));
    let mut active = vec![false; declarations.len()];
    let mut heap: BinaryHeap<(u64, usize)> = BinaryHeap::new();
    let mut units = Vec::new();
    let mut previous_offset = events.first().map_or(0, |event| event.0);
    let mut fragment: Option<(usize, usize, usize)> = None;
    let mut scanned_offset = 0;
    let mut line = 1;
    let deadline = Instant::now() + Duration::from_millis(250);
    let limited = std::cell::Cell::new(false);
    let mut finish = |fragment: &mut Option<(usize, usize, usize)>| -> bool {
        let Some((owner, start, end)) = fragment.take() else {
            return true;
        };
        if text[start..end]
            .iter()
            .all(|character| chunking::whitespace(*character))
        {
            return true;
        }
        if units.len() >= 5000 {
            limited.set(true);
            return false;
        }
        while scanned_offset < start {
            if text[scanned_offset] == 10 {
                line += 1;
            }
            scanned_offset += 1;
        }
        let start_line = line;
        while scanned_offset < end {
            if text[scanned_offset] == 10 {
                line += 1;
            }
            scanned_offset += 1;
        }
        let mut unit = declarations[owner]["unit"].clone();
        unit["unitStartOffset"] = unit["startOffset"].clone();
        unit["unitEndOffset"] = unit["endOffset"].clone();
        unit["startOffset"] = json!(start);
        unit["endOffset"] = json!(end);
        unit["startLine"] = json!(start_line);
        unit["endLine"] = json!(line);
        units.push(unit);
        true
    };
    let mut index = 0;
    while index < events.len() {
        if Instant::now() > deadline {
            limited.set(true);
            break;
        }
        while heap.peek().is_some_and(|(_, owner)| !active[*owner]) {
            heap.pop();
        }
        let owner = heap.peek().map(|(_, owner)| *owner);
        let offset = events[index].0;
        if offset > previous_offset {
            if let Some(owner) = owner {
                if fragment.is_some_and(|(previous_owner, _, end)| {
                    previous_owner == owner && end == previous_offset
                }) {
                    if let Some((_, _, end)) = &mut fragment {
                        *end = offset;
                    }
                } else {
                    if !finish(&mut fragment) {
                        return Ok(json!({"units":units,"limited":true}));
                    }
                    fragment = Some((owner, previous_offset, offset));
                }
            } else if !finish(&mut fragment) {
                return Ok(json!({"units":units,"limited":true}));
            }
        }
        while index < events.len() && events[index].0 == offset {
            let (_, is_active, owner) = events[index];
            index += 1;
            active[owner] = is_active;
            if is_active {
                heap.push((declarations[owner]["depth"].as_u64().unwrap_or(0), owner));
            }
        }
        previous_offset = offset;
    }
    finish(&mut fragment);
    Ok(json!({"units":units,"limited":limited.get()}))
}

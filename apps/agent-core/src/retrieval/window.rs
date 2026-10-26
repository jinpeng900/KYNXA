use serde_json::{Value, json};

const MAX_SOURCE_CHARACTERS: usize = 2 * 1024 * 1024;
const MAX_WINDOW_CHARACTERS: u64 = 16000;
const INVALID_WINDOW: &str = "INVALID_RETRIEVAL_WINDOW";

fn integer(value: &Value, minimum: u64, maximum: u64) -> bool {
    value
        .as_u64()
        .is_some_and(|number| number >= minimum && number <= maximum)
}

fn split_pair(text: &[u16], offset: usize) -> bool {
    offset > 0
        && offset < text.len()
        && (0xd800..=0xdbff).contains(&text[offset - 1])
        && (0xdc00..=0xdfff).contains(&text[offset])
}

fn options(value: &Value) -> Result<Value, String> {
    let mode = value
        .get("mode")
        .cloned()
        .unwrap_or_else(|| json!("window"));
    let before = value
        .get("beforeCharacters")
        .cloned()
        .unwrap_or_else(|| json!(384));
    let limit = value.get("limit").cloned().unwrap_or_else(|| json!(4000));
    if !matches!(mode.as_str(), Some("window" | "section"))
        || value
            .get("anchorOffset")
            .is_some_and(|anchor| !integer(anchor, 0, MAX_SOURCE_CHARACTERS as u64))
        || !integer(&before, 0, 4096)
        || !integer(&limit, 2, MAX_WINDOW_CHARACTERS)
    {
        return Err(INVALID_WINDOW.into());
    }
    let mut result = json!({"mode": mode, "beforeCharacters": before, "limit": limit});
    if let Some(anchor) = value.get("anchorOffset") {
        result["anchorOffset"] = anchor.clone();
    }
    Ok(result)
}

fn preflight(args: &Value) -> Result<(Vec<u16>, Value), String> {
    let text = args["text"]
        .as_str()
        .ok_or_else(|| INVALID_WINDOW.to_owned())?;
    let units: Vec<u16> = text.encode_utf16().collect();
    if units.len() > MAX_SOURCE_CHARACTERS {
        return Err(INVALID_WINDOW.into());
    }
    let normalized = options(&args["options"])?;
    let anchor = normalized["anchorOffset"].as_u64().unwrap_or(0) as usize;
    if anchor > units.len() || split_pair(&units, anchor) {
        return Err("INVALID_RETRIEVAL_OFFSET".into());
    }
    if let Some(reference) = args["options"]
        .get("referenceRange")
        .filter(|value| !value.is_null() && value != &&Value::Bool(false))
    {
        if !integer(&reference["startOffset"], 0, units.len() as u64)
            || !integer(
                &reference["endOffset"],
                reference["startOffset"].as_u64().unwrap_or(0) + 1,
                units.len() as u64,
            )
            || split_pair(
                &units,
                reference["startOffset"].as_u64().unwrap_or(0) as usize,
            )
            || split_pair(
                &units,
                reference["endOffset"].as_u64().unwrap_or(0) as usize,
            )
        {
            return Err(INVALID_WINDOW.into());
        }
    }
    Ok((units, normalized))
}

fn section_end(headings: &[Value], heading: &Value, length: usize) -> usize {
    let offset = heading["startOffset"].as_u64().unwrap_or(0);
    let level = heading["level"].as_u64().unwrap_or(0);
    headings
        .iter()
        .find(|next| {
            next["startOffset"].as_u64().unwrap_or(0) > offset
                && next["level"].as_u64().unwrap_or(0) <= level
        })
        .and_then(|next| next["startOffset"].as_u64())
        .map(|offset| offset as usize)
        .unwrap_or(length)
}

/// Use UTF-16 window/reference offsets and facts from the actual parser.
/// 窗口及引用偏移使用 UTF-16，语法事实取自实际解析器。
pub fn apply(operation: &str, args: &Value) -> Result<Value, String> {
    if operation == "retrieval.window-options" {
        return options(&args["options"]);
    }
    let (text, normalized) = preflight(args)?;
    if operation == "retrieval.window-preflight" {
        return Ok(normalized);
    }
    if operation != "retrieval.source-window" {
        return Err("AGENT_CORE_UNKNOWN_OPERATION".into());
    }
    let anchor = normalized["anchorOffset"].as_u64().unwrap_or(0) as usize;
    let before = normalized["beforeCharacters"].as_u64().unwrap_or(384) as usize;
    let limit = normalized["limit"].as_u64().unwrap_or(4000) as usize;
    let reference = args["options"]
        .get("referenceRange")
        .filter(|value| value.is_object());
    let mut lower = 0;
    let mut upper = text.len();
    let mut section = None;
    let mut sections = None;
    let mut diagnostics = None;
    let mut spans_sections = false;
    let mut navigation_truncated = false;
    if normalized["mode"] == "section" {
        let incomplete = args["parsed"]["incomplete"].as_bool().unwrap_or(false);
        let headings = if incomplete {
            Vec::new()
        } else {
            args["parsed"]["headings"]
                .as_array()
                .cloned()
                .unwrap_or_default()
        };
        if incomplete {
            diagnostics = Some(args["parsed"]["diagnosticCodes"].clone());
        }
        let selected = headings
            .iter()
            .enumerate()
            .take_while(|(_, heading)| {
                heading["startOffset"].as_u64().unwrap_or(0) <= anchor as u64
            })
            .last()
            .map(|(index, _)| index);
        if let Some(index) = selected {
            let heading = &headings[index];
            lower = heading["startOffset"].as_u64().unwrap_or(0) as usize;
            upper = section_end(&headings, heading, text.len());
            let mut value = heading.clone();
            value["endOffset"] = json!(upper);
            section = Some(value);
        }
        if let Some(reference) = reference {
            let start = reference["startOffset"].as_u64().unwrap_or(0);
            let end = reference["endOffset"].as_u64().unwrap_or(0);
            let crossed: Vec<Value> = headings
                .iter()
                .filter(|heading| {
                    let offset = heading["startOffset"].as_u64().unwrap_or(0);
                    offset > start && offset < end
                })
                .cloned()
                .collect();
            spans_sections = !crossed.is_empty();
            if spans_sections {
                let mut navigation = selected
                    .map(|index| vec![headings[index].clone()])
                    .unwrap_or_default();
                navigation.extend(crossed);
                navigation_truncated = navigation.len() > 16;
                sections = Some(
                    navigation
                        .into_iter()
                        .take(16)
                        .map(|mut heading| {
                            heading["endOffset"] =
                                json!(section_end(&headings, &heading, text.len()));
                            heading
                        })
                        .collect::<Vec<_>>(),
                );
                section = None;
                lower = 0;
                upper = text.len();
            }
        }
    }
    let mut start = lower.max(anchor.saturating_sub(before.min(limit - 2)));
    if section.is_some() && upper - lower <= limit {
        start = lower;
    }
    if spans_sections {
        if let Some(reference) = reference {
            let end = reference["endOffset"].as_u64().unwrap_or(0) as usize;
            if end - reference["startOffset"].as_u64().unwrap_or(0) as usize <= limit {
                start = start.max(end.saturating_sub(limit));
            }
        }
    }
    if split_pair(&text, start) {
        start -= 1;
    }
    let mut end = upper.min(start + limit);
    if split_pair(&text, end) {
        end -= 1;
    }
    let mut window = json!({"version":"source-window-v1", "mode": if section.is_some() {"section"} else {"window"},
        "anchorOffset":anchor, "offsetUnit":"utf16-code-units", "startOffset":start, "endOffset":end,
        "clippedAtStart":start > lower, "clippedAtEnd":end < upper});
    if let Some(section) = section {
        window["section"] = section;
    }
    if let Some(sections) = sections {
        window["sections"] = json!(sections);
        window["spansSections"] = json!(spans_sections);
        window["navigationTruncated"] = json!(navigation_truncated);
    }
    if let Some(diagnostics) = diagnostics {
        window["sectionUnavailable"] = json!(true);
        window["navigationTruncated"] = json!(true);
        window["diagnosticCodes"] = diagnostics;
    }
    if let Some(reference) = reference {
        window["referenceRange"] = reference.clone();
        window["referenceRangeCovered"] = json!(
            start <= reference["startOffset"].as_u64().unwrap_or(0) as usize
                && end >= reference["endOffset"].as_u64().unwrap_or(0) as usize
        );
    }
    Ok(
        json!({"text":String::from_utf16(&text[start..end]).map_err(|_| INVALID_WINDOW.to_owned())?,
        "offset":start, "nextOffset":end, "offsetUnit":"utf16-code-units", "totalCharacters":text.len(), "hasMore":end < upper, "window":window}),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn unicode_anchor_and_window_coordinates_stay_utf16() {
        let args =
            json!({"text":"ab😀中文", "options":{"anchorOffset":2,"beforeCharacters":0,"limit":2}});
        let result = apply("retrieval.source-window", &args).unwrap();
        assert_eq!(result["text"], "😀");
        assert_eq!(result["nextOffset"], 4);
        assert_eq!(
            apply(
                "retrieval.source-window",
                &json!({"text":"ab😀", "options":{"anchorOffset":3}})
            ),
            Err("INVALID_RETRIEVAL_OFFSET".into())
        );
    }
    #[test]
    fn crossed_sections_keep_reference_range_and_navigation() {
        let result = apply("retrieval.source-window", &json!({"text":"# A\na\n# B\nb", "options":{"mode":"section","referenceRange":{"startOffset":0,"endOffset":11}},
            "parsed":{"headings":[{"title":"A","level":1,"startOffset":0},{"title":"B","level":1,"startOffset":6}]}})).unwrap();
        assert_eq!(result["window"]["spansSections"], true);
        assert_eq!(result["window"]["referenceRangeCovered"], true);
        assert_eq!(result["window"]["sections"].as_array().unwrap().len(), 2);
    }
}

use std::time::{Duration, Instant};

use serde_json::{Value, json};

use super::{chunking, query};

fn trim(text: &str) -> &str {
    text.trim_matches(|character: char| {
        character.len_utf16() == 1 && chunking::whitespace(character as u16)
    })
}
fn title(text: &str) -> String {
    let normalized = query::pattern(r"[\r\n\t ]+").replace_all(text, " ");
    let units = trim(&normalized).encode_utf16().collect::<Vec<_>>();
    let mut end = units.len().min(128);
    if end < units.len() && end > 0 && (0xd800..=0xdbff).contains(&units[end - 1]) {
        end -= 1;
    }
    String::from_utf16_lossy(&units[..end])
}
fn atx(text: &str) -> Option<(usize, String)> {
    let captures = query::pattern(r"^ {0,3}(#{1,6})(?:[\t ]+|$)(.*)$").captures(text)?;
    let raw = query::pattern(r"[\t ]+#+[\t ]*$").replace(&captures[2], "");
    Some((captures[1].len(), title(&raw)))
}
fn setext(text: &str) -> Option<usize> {
    query::pattern(r"^ {0,3}(=+|-+)[\t ]*$")
        .captures(text)
        .map(|capture| if capture[1].starts_with('=') { 1 } else { 2 })
}
fn fence(text: &str) -> Option<(char, usize, String)> {
    query::pattern(r"^ {0,3}(`{3,}|~{3,})(.*)$")
        .captures(text)
        .map(|capture| {
            (
                capture[1].chars().next().unwrap_or('`'),
                capture[1].len(),
                capture[2].to_owned(),
            )
        })
}
fn table_pipe(text: &str) -> bool {
    let mut previous = None;
    for character in text.chars() {
        if character == '|' && previous != Some('\\') {
            return true;
        }
        previous = Some(character);
    }
    false
}
fn table_delimiter(text: &str) -> bool {
    query::pattern(r"^ {0,3}\|?[\t ]*:?-+:?[\t ]*(?:\|[\t ]*:?-+:?[\t ]*)+\|?[\t ]*$")
        .is_match(text)
}
fn setext_eligible(text: &str) -> bool {
    !trim(text).is_empty()
        && !query::pattern(r"^(?: {4}|\t)|^\s*(?:[-+*]\s|\d+[.)]\s|>)").is_match(text)
}
fn line(lines: &[Value], index: usize) -> &str {
    lines[index]["text"].as_str().unwrap_or("")
}

struct Blocks<'a> {
    lines: &'a [Value],
    units: Vec<Value>,
    titles: Vec<Option<String>>,
    limited: bool,
}

impl Blocks<'_> {
    fn append(&mut self, kind: &str, start: usize, end: usize, heading: Option<(usize, String)>) {
        if self.units.len() >= 5000 {
            self.limited = true;
            return;
        }
        if start >= end || end > self.lines.len() {
            return;
        }
        if let Some((level, title)) = &heading {
            self.titles.resize(*level, None);
            self.titles[level - 1] = Some(title.clone());
        }
        let path = self
            .titles
            .iter()
            .flatten()
            .filter(|title| !title.is_empty())
            .cloned()
            .collect::<Vec<_>>();
        let first = &self.lines[start];
        let last = &self.lines[end - 1];
        let mut unit = json!({"kind":kind,"startOffset":first["startOffset"],"endOffset":last["endOffset"],"startLine":first["startLine"],"endLine":last["endLine"]});
        if let Some(title) = path.last() {
            unit["sectionTitle"] = json!(title);
            unit["sectionPath"] = json!(path);
        }
        if let Some((level, title)) = heading {
            unit["headingLevel"] = json!(level);
            unit["sectionTitle"] = json!(title);
        }
        self.units.push(unit);
    }
}

/// Recognize only supported Markdown/text blocks; untouched gaps remain explicit raw context upstream.
/// 只识别已支持的 Markdown/文本块；未解析区间由上层保留原文，不能扩大语法覆盖声明。
pub fn parse(args: &Value) -> Result<Value, String> {
    let lines = args["lines"]
        .as_array()
        .filter(|lines| lines.len() <= 100000)
        .ok_or("INVALID_RETRIEVAL_STRUCTURE")?;
    let extension = args["extension"].as_str().unwrap_or("");
    let markdown = if [".md", ".markdown"].contains(&extension) {
        true
    } else if !extension.is_empty() {
        false
    } else {
        lines.iter().enumerate().any(|(index, _)| {
            atx(line(lines, index)).is_some()
                || fence(line(lines, index)).is_some()
                || index > 0
                    && setext(line(lines, index)).is_some()
                    && setext_eligible(line(lines, index - 1))
        })
    };
    let mut blocks = Blocks {
        lines,
        units: Vec::new(),
        titles: Vec::new(),
        limited: args["complete"] != true,
    };
    let mut partial = blocks.limited;
    let mut diagnostics = Vec::new();
    let mut index = 0;
    let deadline = Instant::now() + Duration::from_millis(250);
    while index < lines.len() {
        if blocks.units.len() >= 5000 || Instant::now() > deadline {
            blocks.limited = true;
            break;
        }
        let current = line(lines, index);
        if trim(current).is_empty() {
            index += 1;
            continue;
        }
        if !markdown {
            let start = index;
            index += 1;
            while index < lines.len() && !trim(line(lines, index)).is_empty() {
                if index % 256 == 0 && Instant::now() > deadline {
                    blocks.limited = true;
                    break;
                }
                index += 1;
            }
            blocks.append("paragraph", start, index, None);
            continue;
        }
        if let Some((marker, length, suffix)) =
            fence(current).filter(|(marker, _, suffix)| !(*marker == '`' && suffix.contains('`')))
        {
            let start = index;
            index += 1;
            let mut closed = false;
            while index < lines.len() {
                if Instant::now() > deadline {
                    blocks.limited = true;
                    break;
                }
                if fence(line(lines, index)).is_some_and(|(closing, closing_length, info)| {
                    closing == marker && closing_length >= length && trim(&info).is_empty()
                }) {
                    index += 1;
                    closed = true;
                    break;
                }
                index += 1;
            }
            if !closed {
                partial = true;
                if !blocks.limited {
                    diagnostics.push("UNTERMINATED_CODE_FENCE");
                }
            }
            let _ = suffix;
            blocks.append("code-block", start, index, None);
            continue;
        }
        if let Some(heading) = atx(current) {
            blocks.append("heading", index, index + 1, Some(heading));
            index += 1;
            continue;
        }
        if index + 1 < lines.len() && table_pipe(current) && table_delimiter(line(lines, index + 1))
        {
            let start = index;
            index += 2;
            while index < lines.len()
                && !trim(line(lines, index)).is_empty()
                && table_pipe(line(lines, index))
                && atx(line(lines, index)).is_none()
                && fence(line(lines, index)).is_none()
            {
                if index % 256 == 0 && Instant::now() > deadline {
                    blocks.limited = true;
                    break;
                }
                index += 1;
            }
            blocks.append("table", start, index, None);
            continue;
        }
        let start = index;
        index += 1;
        let mut setext_heading = None;
        while index < lines.len() && !trim(line(lines, index)).is_empty() {
            if Instant::now() > deadline {
                blocks.limited = true;
                break;
            }
            let current = line(lines, index);
            if let Some(level) = setext(current).filter(|_| setext_eligible(line(lines, index - 1)))
            {
                if index - start > 1 {
                    blocks.append("paragraph", start, index - 1, None);
                }
                setext_heading = Some((index - 1, level, title(line(lines, index - 1))));
                index += 1;
                break;
            }
            if atx(current).is_some()
                || fence(current).is_some()
                || index + 1 < lines.len()
                    && table_pipe(current)
                    && table_delimiter(line(lines, index + 1))
            {
                break;
            }
            index += 1;
        }
        if let Some((heading_start, level, title)) = setext_heading {
            blocks.append("heading", heading_start, index, Some((level, title)));
        } else {
            blocks.append("paragraph", start, index, None);
        }
    }
    if blocks.limited {
        partial = true;
        diagnostics.push("DOCUMENT_STRUCTURE_LIMIT");
    }
    Ok(
        json!({"domain":"knowledge","language":if markdown {"markdown"} else {"text"},"parserVersion":if markdown {"markdown-blocks-v2"} else {"plain-paragraphs-v2"},
        "parseStatus":if partial {"partial"} else {"parsed"},"diagnosticCodes":diagnostics,"units":blocks.units}),
    )
}

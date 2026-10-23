use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};

use regex::Regex;
use serde_json::{Value, json};

// Patterns originate in the existing gateway contract; ASCII word boundaries remain explicit.
// 模式沿用已有网关合同；显式保留 JavaScript 的 ASCII 词边界，避免中文相邻字符改变匹配。
pub(super) fn pattern(source: &str) -> Regex {
    static PATTERNS: OnceLock<Mutex<HashMap<String, Regex>>> = OnceLock::new();
    let mut patterns = PATTERNS
        .get_or_init(|| Mutex::new(HashMap::new()))
        .lock()
        .unwrap();
    patterns
        .entry(source.to_owned())
        .or_insert_with(|| {
            let expression = compatible_pattern(source);
            Regex::new(&format!("(?i:{expression})")).expect("validated static tool pattern")
        })
        .clone()
}

fn compatible_pattern(source: &str) -> String {
    let source = source.replace(r"\p{Script=Han}", r"\p{Han}");
    let mut characters = source.chars();
    let mut output = String::new();
    let mut in_class = false;
    while let Some(ch) = characters.next() {
        if ch == '[' {
            in_class = true;
        }
        if ch == ']' {
            in_class = false;
        }
        if ch != '\\' {
            output.push(ch);
            continue;
        }
        let Some(escape) = characters.next() else {
            output.push(ch);
            break;
        };
        let contents = match escape {
            'w' => Some("A-Za-z0-9_"),
            's' => Some(JS_WHITESPACE),
            _ => None,
        };
        if let Some(contents) = contents {
            if !in_class {
                output.push('[');
            }
            output.push_str(contents);
            if !in_class {
                output.push(']');
            }
        } else if escape == 'S' && !in_class {
            output.push_str(&format!("[^{JS_WHITESPACE}]"));
        } else if escape == 'b' && !in_class {
            output.push_str(r"(?-u:\b)");
        } else {
            output.push('\\');
            output.push(escape);
        }
    }
    output
}

const JS_WHITESPACE: &str = r"\t\n\x0b\x0c\r \u{00a0}\u{1680}\u{2000}-\u{200a}\u{2028}\u{2029}\u{202f}\u{205f}\u{3000}\u{feff}";

pub(super) fn matches(source: &str, text: &str) -> bool {
    pattern(source).is_match(text)
}

pub(super) fn utf16_length(text: &str) -> usize {
    text.encode_utf16().count()
}

pub(super) fn js_trim(text: &str) -> &str {
    text.trim_matches(|ch| {
        matches!(
            ch,
            '\t' | '\n'
                | '\u{000b}'
                | '\u{000c}'
                | '\r'
                | ' '
                | '\u{00a0}'
                | '\u{1680}'
                | '\u{2000}'
                ..='\u{200a}'
                    | '\u{2028}'
                    | '\u{2029}'
                    | '\u{202f}'
                    | '\u{205f}'
                    | '\u{3000}'
                    | '\u{feff}'
        )
    })
}

fn offset(text: &str, byte_offset: usize) -> usize {
    utf16_length(&text[..byte_offset])
}

#[derive(Clone)]
struct QuoteSpan {
    start: usize,
    end: usize,
    content_start: usize,
    content_end: usize,
    closed: bool,
}

fn quoted_spans(text: &str) -> Vec<QuoteSpan> {
    let mut spans = Vec::new();
    let mut cursor = 0;
    while cursor < text.len() {
        let opener = text[cursor..].chars().next().unwrap();
        let closer = match opener {
            '“' => "”",
            '‘' => "’",
            '「' => "」",
            '『' => "』",
            '"' => "\"",
            '\'' => "'",
            '`' => "`",
            _ => {
                cursor += opener.len_utf8();
                continue;
            }
        };
        let apostrophe = |position: usize| {
            text[..position]
                .chars()
                .next_back()
                .is_some_and(char::is_alphanumeric)
                && text[position + 1..]
                    .chars()
                    .next()
                    .is_some_and(char::is_alphanumeric)
        };
        if opener == '\'' && apostrophe(cursor) {
            cursor += 1;
            continue;
        }
        let delimiter = if opener == '`' {
            &text[cursor..cursor + text[cursor..].bytes().take_while(|b| *b == b'`').count()]
        } else {
            &text[cursor..cursor + opener.len_utf8()]
        };
        let closer = if opener == '`' { delimiter } else { closer };
        let content_start = cursor + delimiter.len();
        let mut end = content_start;
        while end < text.len() {
            if text[end..].starts_with('\\') {
                end += 1;
                if end < text.len() {
                    end += text[end..].chars().next().unwrap().len_utf8();
                }
                continue;
            }
            if text[end..].starts_with(closer) && !(closer == "'" && apostrophe(end)) {
                break;
            }
            end += text[end..].chars().next().unwrap().len_utf8();
        }
        let closed = end < text.len();
        spans.push(QuoteSpan {
            start: cursor,
            end: if closed {
                end + closer.len()
            } else {
                text.len()
            },
            content_start,
            content_end: end,
            closed,
        });
        cursor = spans.last().unwrap().end;
    }
    spans
}

pub(super) fn instruction_text(text: &str, preserve_browser: bool) -> String {
    let mut projected = String::new();
    let mut cursor = 0;
    for span in quoted_spans(text) {
        projected.push_str(&text[cursor..span.start]);
        let quoted = &text[span.start..span.end];
        if preserve_browser
            && span.closed
            && matches(
                &format!("^{BROWSER_NAME}$"),
                text[span.content_start..span.content_end].trim(),
            )
        {
            projected.push_str(quoted);
        } else {
            projected.extend(
                quoted
                    .chars()
                    .map(|ch| if ch == '\r' || ch == '\n' { ch } else { ' ' }),
            );
        }
        cursor = span.end;
    }
    projected.push_str(&text[cursor..]);
    projected
}

fn clause(text: &str, start: usize, end: usize) -> Option<Value> {
    let raw = &text[start..end];
    let value = raw.trim();
    if value.is_empty() {
        return None;
    }
    let start = start + raw.len() - raw.trim_start().len();
    Some(
        json!({"text": value, "startOffset": offset(text, start), "endOffset": offset(text, start + value.len())}),
    )
}

fn request_clauses(text: &str) -> Vec<Value> {
    let quotes = quoted_spans(text);
    let inside_quote = |position: usize| {
        quotes
            .iter()
            .any(|span| position >= span.start && position < span.end)
    };
    let separators = pattern(r"[，。；：,;!?！？\n]+|\.[ \t\r\n]+|:\s+");
    let mut ranges = Vec::new();
    let mut cursor = 0;
    for separator in separators.find_iter(text) {
        if inside_quote(separator.start()) {
            continue;
        }
        ranges.push((cursor, separator.start()));
        // Only punctuation is consumed by the original lookahead; trimming preserves subsequent offsets.
        // 原模式的前瞻不消费后续空白；这里仅消费标点，保留子句的原始位置。
        cursor = if text[separator.start()..].starts_with('.')
            || text[separator.start()..].starts_with(':')
        {
            separator.start() + 1
        } else {
            separator.end()
        };
    }
    if text.ends_with('.') && cursor < text.len() && !inside_quote(text.len() - 1) {
        ranges.push((cursor, text.len() - 1));
        cursor = text.len();
    }
    ranges.push((cursor, text.len()));
    let contrast = pattern(r"不是|并非|而是|改(?:为|成)|\bbut\b|\binstead\b");
    let mut clauses = Vec::new();
    for (start, end) in ranges {
        let segment = &text[start..end];
        let mut previous = 0;
        for boundary in contrast.find_iter(segment) {
            if boundary.start() == 0 || inside_quote(start + boundary.start()) {
                continue;
            }
            let tail = &segment[boundary.end()..];
            if matches!(boundary.as_str().to_lowercase().as_str(), "but" | "instead")
                && (!matches(r"^\s+\S", tail)
                    || boundary.as_str().eq_ignore_ascii_case("instead")
                        && matches(r"^\s+of\b", tail))
            {
                continue;
            }
            if let Some(value) = clause(text, start + previous, start + boundary.start()) {
                clauses.push(value);
            }
            previous = boundary.start();
        }
        if let Some(value) = clause(text, start + previous, end) {
            clauses.push(value);
        }
    }
    clauses
}

pub(super) fn analyze_clauses(text: &str) -> Value {
    let mut active = Vec::<Value>::new();
    let mut excluded = Vec::<Value>::new();
    let mut protected_negative = 0usize;
    let mut replacement_context = false;
    let mut boundary = "none";
    let mut reason = None;
    for mut clause in request_clauses(text) {
        let instruction = instruction_text(clause["text"].as_str().unwrap(), false);
        let instruction = pattern(REQUEST_LEAD)
            .replace(instruction.trim(), "")
            .to_string();
        let exit = matches(TASK_EXIT, &instruction) || matches(CANCELLED_TASK, &instruction);
        let negative = matches(NEGATED_CLAUSE, &instruction);
        let retained = matches(RETAINED_NEGATION, &instruction);
        let is_excluded = exit
            || !retained
                && (matches(NEGATED_ACTION, &instruction)
                    || matches(NEGATED_SOURCE, &instruction)
                    || negative && replacement_context);
        let protected = negative && !is_excluded;
        let correction = !protected && matches(CORRECTION, &instruction);
        let supersede = if exit || matches(TOPIC_SWITCH, &instruction) {
            boundary = "topic-switch";
            reason = Some(if exit {
                "explicit-task-exit"
            } else {
                "explicit-topic-switch"
            });
            true
        } else if (is_excluded || correction) && boundary != "topic-switch" {
            boundary = "correction";
            reason = Some("explicit-user-replacement");
            !is_excluded && correction && protected_negative == 0
        } else {
            false
        };
        if supersede {
            for mut previous in active.drain(..) {
                previous["basis"] = json!("superseded-by-task-boundary");
                excluded.push(previous);
            }
            protected_negative = 0;
        }
        replacement_context |= is_excluded || correction;
        if is_excluded {
            clause["basis"] = json!(if exit {
                "explicit-task-exit"
            } else if matches(CORRECTION, &instruction) {
                "explicit-user-replacement"
            } else {
                "explicit-negation"
            });
            excluded.push(clause);
        } else {
            active.push(clause);
            if protected {
                protected_negative += 1;
            }
        }
    }
    let active_text = if excluded.is_empty() && boundary == "none" {
        text.to_owned()
    } else {
        active
            .iter()
            .filter_map(|value| value["text"].as_str())
            .collect::<Vec<_>>()
            .join("，")
    };
    let provenance = excluded.iter().map(|clause| json!({"rejected": clause["text"], "accepted": active_text, "basis": clause["basis"]})).collect::<Vec<_>>();
    let mut projection = json!({"activeText": active_text, "excludedClauses": excluded, "boundary": boundary, "provenance": provenance, "originalPreserved": true, "semanticVerified": false});
    if let Some(reason) = reason {
        projection["boundaryReason"] = json!(reason);
    }
    projection
}

pub(super) fn task_relation(text: &str) -> Value {
    let clauses = analyze_clauses(text);
    let active = clauses["activeText"].as_str().unwrap().trim();
    let (kind, inheritance, reason) = if clauses["boundary"] != "none" {
        (
            clauses["boundary"].as_str().unwrap(),
            false,
            clauses["boundaryReason"]
                .as_str()
                .unwrap_or("explicit-task-boundary"),
        )
    } else if !clauses["excludedClauses"].as_array().unwrap().is_empty() {
        ("uncertain", false, "explicit-clause-exclusion")
    } else if matches(SUPPLEMENT, active) {
        ("supplement", true, "explicit-supplement")
    } else if matches(CONTINUATION_REFERENCE, active)
        || utf16_length(active) <= 240 && matches(EXPLICIT_RETRY, active)
    {
        ("continue", true, "continuation-reference")
    } else {
        ("new", false, "no-continuation-reference")
    };
    json!({"type":kind, "allowsInheritance":inheritance, "reason":reason})
}

pub(super) fn path_domain(path: &str) -> &'static str {
    let extension = pattern(r"\.([a-z0-9]+)$")
        .captures(path)
        .map(|captures| captures[1].to_lowercase())
        .unwrap_or_default();
    if CODE_EXTENSIONS.contains(&extension.as_str()) {
        "code"
    } else if DOCUMENT_EXTENSIONS.contains(&extension.as_str()) {
        "knowledge"
    } else {
        "mixed"
    }
}

pub(super) fn file_references(text: &str) -> Value {
    let extensions = CODE_EXTENSIONS
        .iter()
        .chain(DOCUMENT_EXTENSIONS.iter())
        .copied()
        .chain(["json", "yaml", "yml", "xml", "toml", "ini", "csv"])
        .collect::<Vec<_>>()
        .join("|");
    let files = pattern(&format!(
        r"(?:[A-Za-z]:[\\/])?(?:[\w\p{{Han}}.-]+[\\/])*[\w\p{{Han}}.-]+\.(?:{extensions})\b"
    ));
    let urls = pattern(r#"https?://[^\s`"'<>]+"#)
        .find_iter(text)
        .map(|item| (item.start(), item.end()))
        .collect::<Vec<_>>();
    let mut references = Vec::new();
    for file in files.find_iter(text) {
        let preceding = text[..file.start()].chars().next_back();
        if preceding.is_some_and(|ch| {
            ch.is_ascii_alphanumeric()
                || "_./\\-".contains(ch)
                || matches(r"\p{Han}", &ch.to_string())
        }) || urls
            .iter()
            .any(|(start, end)| file.start() >= *start && file.start() < *end)
        {
            continue;
        }
        let value = pattern(FILE_ACTION_LEAD)
            .replace(file.as_str(), "")
            .to_string();
        let start = offset(text, file.end()) - utf16_length(&value);
        references.push(json!({"value":value, "startOffset":start, "endOffset":start + utf16_length(&value), "domain":path_domain(&value)}));
    }
    json!(references)
}

pub(super) fn calculate(operation: &str, arguments: &Value) -> Result<Value, String> {
    let text = arguments["message"].as_str().unwrap_or("");
    Ok(match operation {
        "tools.requestClauses" => analyze_clauses(text),
        "tools.taskRelation" => task_relation(text),
        "tools.fileReferences" => file_references(text),
        "tools.pathDomain" => json!(path_domain(arguments["path"].as_str().unwrap_or(""))),
        "tools.quoteSpans" => json!(quoted_spans(text).iter().map(|span| json!({"start":offset(text,span.start), "end":offset(text,span.end), "content":&text[span.content_start..span.content_end], "closed":span.closed})).collect::<Vec<_>>()),
        "tools.browserFollowup" => json!(browser_followup(text)),
        "tools.browserConnection" => connection_data(&arguments["server"]),
        "tools.backgroundBrowserConnection" => json!(background_connection(arguments)),
        "tools.foregroundForbidden" => json!(foreground_forbidden(text, arguments.get("target"))),
        "tools.targetedForegroundRestriction" => json!(foreground_forbidden(&current_browser_instruction(text), None) && matches(r"浏览器|\bbrowser\b|chrome|edge|firefox|终端|powershell|\bcmd\b|terminal|记事本|notepad|(?:激活|focus|activate)\s*[^\s，。；,.!?;]{2,}",&current_browser_instruction(text))),
        "tools.browserInteraction" => browser_interaction(text),
        "tools.browserApplication" => json!(is_browser_executable(arguments["executable"].as_str().unwrap_or(""))),
        "tools.browserIntent" => browser_intent(text, arguments["previousUserMessages"].as_array().map(Vec::as_slice).unwrap_or(&[])),
        "tools.browserExecutable" => json!(configured_executable(arguments["args"].as_array().map(Vec::as_slice).unwrap_or(&[]), arguments["engine"].as_str().unwrap_or(""))),
        "tools.browserAllowed" => {
            let intent = &arguments["intent"];
            let excluded = intent["excludedBrowserExecutables"].as_array().is_some_and(|values| values.iter().any(|value| value.as_str() == arguments["executable"].as_str()));
            json!(!excluded && (intent["allowLocalBrowser"] == true || arguments["background"] == true))
        },
        _ => return Err("AGENT_CORE_UNKNOWN_OPERATION".to_owned()),
    })
}

fn connection_option<'a>(args: &'a [Value], names: &[&str]) -> Option<&'a str> {
    for (index, value) in args.iter().enumerate() {
        let value = value.as_str().unwrap_or("");
        for name in names {
            if value == *name {
                return Some(args.get(index + 1).and_then(Value::as_str).unwrap_or(""));
            }
            if let Some(value) = value.strip_prefix(&format!("{name}=")) {
                return Some(value);
            }
        }
    }
    None
}

fn connection_data(server: &Value) -> Value {
    if server["transport"].as_str().unwrap_or("stdio") != "stdio" {
        return json!({"connection":null});
    }
    let args = server["args"].as_array().map(Vec::as_slice).unwrap_or(&[]);
    let executable = server["command"]
        .as_str()
        .unwrap_or("")
        .rsplit(['/', '\\'])
        .next()
        .unwrap_or("");
    let label = format!(
        "{} {}",
        args.iter()
            .filter_map(Value::as_str)
            .map(|value| value.replace('\\', "/"))
            .collect::<Vec<_>>()
            .join(" "),
        executable
    );
    let engine = if matches(
        r"(?-i:(?:^|[\s/])@playwright/mcp(?:@|[\s/]|$)|playwright-mcp)",
        &label,
    ) {
        "playwright"
    } else if matches(
        r"(?-i:(?:^|[\s/])chrome-devtools-mcp(?:@|[\s/]|\.(?:cmd|exe)(?:\s|$)|$))",
        &label,
    ) {
        "chrome-devtools"
    } else {
        return json!({"connection":null});
    };
    let endpoint = connection_option(
        args,
        if engine == "playwright" {
            &["--cdp-endpoint", "--endpoint"]
        } else {
            &[
                "--browserUrl",
                "--browser-url",
                "-u",
                "--wsEndpoint",
                "--ws-endpoint",
                "-w",
            ]
        },
    );
    let attached = args.iter().filter_map(Value::as_str).any(|value| {
        matches(
            r"(?-i:^(?:--autoConnect|--auto-connect|--extension)(?:=true)?$)",
            value,
        )
    });
    let mut mode = if attached {
        "existing-browser"
    } else if endpoint.is_some() {
        "remote-browser"
    } else {
        "independent-browser"
    };
    if args.iter().filter_map(Value::as_str).any(|value| value == "--config" || value.starts_with("--config="))
        || engine == "playwright" && server["envKeys"].as_array().is_some_and(|keys| keys.iter().filter_map(Value::as_str).any(|key| matches(r"(?-i:^PLAYWRIGHT_MCP_(?:CDP_ENDPOINT|ENDPOINT|EXTENSION|CONFIG|HEADLESS|ISOLATED|USER_DATA_DIR)$)",key))) { mode = "custom-browser"; }
    json!({"connection":{"engine":engine,"mode":mode,"headless":args.iter().filter_map(Value::as_str).any(|value| matches(r"(?-i:^--headless(?:=true)?$)",value)),
        "isolated":args.iter().filter_map(Value::as_str).any(|value| matches(r"(?-i:^--isolated(?:=true)?$)",value))},"endpoint":endpoint})
}

fn background_connection(arguments: &Value) -> bool {
    let server = &arguments["server"];
    let data = connection_data(server);
    let connection = &data["connection"];
    let args = server["args"].as_array().map(Vec::as_slice).unwrap_or(&[]);
    if connection["mode"] == "independent-browser" {
        let disabled = args.iter().enumerate().any(|(index, value)| {
            value.as_str().is_some_and(|value| {
                matches(r"(?-i:^--(?:headless|isolated)=(?:false|0)$)", value)
                    || matches(r"(?-i:^(?:--headless|--isolated)$)", value)
                        && args
                            .get(index + 1)
                            .and_then(Value::as_str)
                            .is_some_and(|next| ["false", "0"].contains(&next))
            })
        });
        return connection["headless"] == true && connection["isolated"] == true && !disabled;
    }
    if connection["mode"] != "remote-browser" {
        return false;
    }
    let endpoint = &arguments["endpointUrl"];
    let hostname = endpoint["hostname"]
        .as_str()
        .unwrap_or("")
        .trim_matches(['[', ']'])
        .trim_end_matches('.')
        .to_lowercase();
    endpoint["protocol"]
        .as_str()
        .is_some_and(|protocol| ["http:", "https:", "ws:", "wss:"].contains(&protocol))
        && !matches(
            r"^(?:localhost|127(?:\.[0-9]{1,3}){3}|::1|0\.0\.0\.0|::)$",
            &hostname,
        )
        && !hostname.ends_with(".localhost")
}

fn foreground_forbidden(message: &str, target: Option<&Value>) -> bool {
    let text = current_browser_instruction(message);
    for clause in pattern(r"[，。；,.!?;\n]").split(&text) {
        if matches(
            r"(?:并)?不是(?:说)?(?:不能|不允许|禁止)|并非(?:不能|不允许|禁止)|不(?:代表|意味着)(?:不能|不允许)",
            clause,
        ) {
            continue;
        }
        if !matches(
            r"(?:不要|别|勿|禁止|不能|不允许)[^，。；,.!?;\n]{0,24}(?:抢.{0,3}焦点|激活|切.{0,3}前台|置.{0,3}前台)|保持.{0,3}后台|(?:仅|只能|始终).{0,3}后台|(?:do not|don't|never|without)[^,.!?;\n]{0,50}(?:focus|activat(?:e|ing)|bring.{0,40}(?:front|foreground))",
            clause,
        ) {
            continue;
        }
        let Some(target) = target.filter(|value| !value.is_null()) else {
            return true;
        };
        let raw_name = target["appPath"]
            .as_str()
            .or_else(|| target["applicationName"].as_str())
            .or_else(|| target["title"].as_str())
            .unwrap_or("");
        let name = raw_name.rsplit(['/', '\\']).next().unwrap_or("");
        let name = pattern(r"\.exe$").replace(name, "").to_string();
        if matches(
            r"浏览器|\bbrowser\b|chrome|chromium|edge|firefox|brave",
            clause,
        ) {
            let brands = pattern("chrome|chromium|edge|firefox|brave")
                .find_iter(clause)
                .map(|value| value.as_str().to_lowercase())
                .collect::<Vec<_>>();
            if !brands.is_empty() && !matches(r"浏览器|\bbrowser\b", clause) && !name.is_empty()
            {
                if brands
                    .iter()
                    .any(|brand| name.to_lowercase().contains(brand))
                {
                    return true;
                }
                continue;
            }
            if target["kind"] == "browser"
                || matches(
                    r"浏览器|\bbrowser\b|chrome|chromium|edge|firefox|brave",
                    &name,
                )
            {
                return true;
            }
            continue;
        }
        if matches(
            r"终端|命令行|powershell|\bcmd\b|\bterminal\b|控制台",
            clause,
        ) {
            if target["kind"] == "terminal"
                || matches(r"终端|命令行|powershell|\bcmd\b|\bterminal\b|控制台", &name)
            {
                return true;
            }
            continue;
        }
        if matches("记事本|notepad", clause) {
            if matches("记事本|notepad", &name) {
                return true;
            }
            continue;
        }
        if matches(
            r"抢.{0,3}焦点|保持.{0,3}后台|(?:仅|只能|始终).{0,3}后台",
            clause,
        ) {
            return true;
        }
        if matches(
            r"(?:激活|focus|activate)\s*(?:当前|这个|该)?\s*(?:窗口|软件|应用|window|application|app)\s*$",
            clause,
        ) {
            return true;
        }
        if !matches(r"(?:激活|focus|activate)\s*[^\s]{2,}", clause)
            || !name.is_empty() && clause.to_lowercase().contains(&name.to_lowercase())
        {
            return true;
        }
    }
    false
}

fn browser_interaction(message: &str) -> Value {
    let text = current_browser_instruction(message);
    let forbidden = foreground_forbidden(message, Some(&json!({"kind":"browser"})));
    let foreground = !forbidden && pattern(r"[，。；,.!?;\n]").split(&text).any(|clause| !matches(r"(?:不要|不必|无需|不能|别|do not|don't|without)",clause)
        && matches(r"(?:切换?到|切到|置于|放到|带到|显示在).{0,3}前台|激活.{0,8}(?:浏览器|窗口|Chrome|Edge)|(?:focus|activate).{0,15}(?:browser|window|chrome|edge)|bring.{0,15}(?:browser|window|chrome|edge|it).{0,15}(?:front|foreground)",clause));
    json!({"background":!foreground,"allowForeground":!forbidden})
}

const CODE_EXTENSIONS: &[&str] = &[
    "cs", "js", "mjs", "cjs", "jsx", "ts", "mts", "cts", "tsx", "py", "pyi", "rs", "go", "java",
    "c", "cpp", "cxx", "cc", "h", "hpp", "hxx", "ps1", "psm1", "sh", "bat", "cmd", "sql", "xaml",
    "html", "css",
];
const DOCUMENT_EXTENSIONS: &[&str] = &["md", "markdown", "txt", "pdf", "docx", "png", "jpg", "jpeg", "bmp", "gif", "tif", "tiff", "webp"];

fn current_browser_instruction(text: &str) -> String {
    let units = text.encode_utf16().collect::<Vec<_>>();
    let tail = String::from_utf16_lossy(&units[units.len().saturating_sub(8000)..]);
    let unfenced = pattern(r"(?s:```.*?```)").replace_all(&tail, "");
    pattern(r"(?m:^>.*$)")
        .replace_all(&unfenced, "")
        .to_string()
}

fn browser_instruction(text: &str) -> String {
    let projected = instruction_text(&current_browser_instruction(text), true);
    pattern(&format!(r#"["“‘「『']\s*({BROWSER_NAME})\s*["”’」』']"#))
        .replace_all(&projected, "$1")
        .to_string()
}

fn executable_name(name: &str) -> Option<&'static str> {
    match name.to_lowercase().as_str() {
        "chrome" => Some("chrome.exe"),
        "msedge" | "edge" => Some("msedge.exe"),
        "firefox" => Some("firefox.exe"),
        "chromium" => Some("chromium.exe"),
        "brave" => Some("brave.exe"),
        "opera" => Some("opera.exe"),
        "vivaldi" => Some("vivaldi.exe"),
        _ => None,
    }
}

fn forbidden_executables(clause: &str) -> Vec<String> {
    let instruction = browser_instruction(clause);
    let mut excluded = Vec::new();
    for restriction in pattern(LOCAL_BROWSER_FORBIDDEN).captures_iter(&instruction) {
        let prefix = restriction
            .name("objectPrefix")
            .or_else(|| restriction.name("useObjectPrefix"))
            .map_or("", |value| value.as_str());
        if matches(
            r"窗口|标签(?:页)?|页面|网页|\b(?:windows?|tabs?|pages?)\b",
            prefix,
        ) {
            continue;
        }
        let mut suffix = &instruction[restriction.get(0).unwrap().end()..];
        let initial = restriction
            .name("browserObject")
            .or_else(|| restriction.name("useBrowserObject"))
            .unwrap()
            .as_str()
            .to_owned();
        let mut objects = vec![(initial, matches(WINDOW_OBJECT, suffix))];
        loop {
            let remaining = if let Some(window) = pattern(WINDOW_OBJECT).find(suffix) {
                &suffix[window.end()..]
            } else {
                suffix
            };
            let next_pattern = pattern(COORDINATED_BROWSER_OBJECT);
            let Some(next) = next_pattern.captures(remaining) else {
                break;
            };
            suffix = &remaining[next.get(0).unwrap().end()..];
            objects.push((
                next.name("browserObject").unwrap().as_str().to_owned(),
                matches(WINDOW_OBJECT, suffix),
            ));
        }
        // Shared trailing windows constrain the group; an earlier per-brand window cannot erase a later app ban.
        // 末尾共享窗口限定整组；前面品牌的独立窗口不能取消后续品牌的应用禁令。
        let shared_window = objects.len() > 1
            && objects.last().unwrap().1
            && objects[..objects.len() - 1].iter().all(|object| !object.1);
        for (browser, scoped) in objects {
            if scoped || shared_window {
                continue;
            }
            if let Some(executable) = executable_name(&browser) {
                if !excluded.iter().any(|value| value == executable) {
                    excluded.push(executable.to_owned());
                }
            }
        }
    }
    excluded
}

fn referenced_instruction(clause: &str, context: &str) -> bool {
    let text = pattern(r"^(?:请\s*)?")
        .replace(clause.trim(), "")
        .to_string();
    matches(
        r"^(?:文档|说明|教程|日志|文章|示例|原文|他|她|他们).{0,16}(?:说|写|要求|建议|提到)|^(?:(?:the|a)\s+)?(?:guide|document|tutorial|log|example|article|he|she|they)\b.{0,24}\b(?:says?|said|asks?|asked|mentions?|mentioned|instructs?)\b",
        &text,
    ) || matches(
        r"^(?:如果|假如|假设|设想).{0,12}(?:我(?:让|要求|请求)|(?:要求|请求|让你))|^(?:if|suppose|imagine|assuming)\b.{0,24}\b(?:asked|were\s+to\s+ask|ask\s+you|asking\s+you)\b",
        &text,
    ) || matches(
        r"^(?:如果|假如|假设|设想)|^(?:if|suppose|imagine|assuming)\b",
        &text,
    ) && matches(
        r"会怎样|会怎么|怎么办|应该怎么|\b(?:what\s+(?:would|happens|should)|how\s+would|would\s+you)\b",
        context,
    )
}

fn browser_followup(message: &str) -> bool {
    let text = browser_instruction(message);
    let text = text.trim();
    if matches(INFORMATIONAL_QUESTION, text) && !matches(EXPLICIT_EXECUTION_REQUEST, text) {
        return false;
    }
    if matches(RESEARCH_PREFIX, text)
        && !matches(
            r"(?:当前|这个|该|本页).{0,8}(?:网页|页面|网站|标签)|作业|账户|账号|二维码",
            text,
        )
    {
        return false;
    }
    matches(
        r"^(?:再来|继续|重试|再次尝试|再次尝试打开|再试(?:一次)?|再试试|再尝试(?:一次)?|再次打开|打不开|还是打不开|try again|retry|continue)[。.!！?？\s]*$",
        text,
    ) || matches(
        r"(?:打开|进入|访问|登录|登陆|刷新|关闭|滚动|截图|截屏|点击|填写|输入|切换|查看|读取|open|navigate|log\s*in|sign\s*in|refresh|reload|close|scroll|screenshot|click|fill|type).{0,60}(?:网站|网页|页面|标签|网址|https?://|学习通|账户|账号|作业|二维码|按钮|输入框|当前|这个|page|tab|website|url|account|form|button)|^(?:向上|向下|往上|往下)?(?:滚动|刷新|关闭当前页面)|^(?:截图|截屏|填写|输入|登录|登陆)",
        text,
    )
}

fn requests_browser_operation(clause: &str) -> bool {
    if matches(INFORMATIONAL_QUESTION, clause) && !matches(EXPLICIT_EXECUTION_REQUEST, clause) {
        return false;
    }
    for source in [USE_BROWSER, IN_BROWSER, BROWSER_SEARCH] {
        for found in pattern(source).find_iter(clause) {
            if !matches(RESEARCH_SUFFIX, &clause[found.end()..]) {
                return true;
            }
        }
    }
    for found in pattern(READ_BROWSER).captures_iter(clause) {
        let remaining = &clause[found.get(0).unwrap().end()..];
        let browser_object = matches(r"浏览器|\bbrowser\b", &found[2])
            || matches(r"^\s*(?:浏览器|\bbrowser\b)", remaining);
        let local = matches(
            r"本机|本地|当前|已打开|我的|自己的|\b(?:local|current|my)\b",
            &found[1],
        );
        if (browser_object || local) && !matches(RESEARCH_SUFFIX, remaining) {
            return true;
        }
    }
    if matches(RESEARCH_PREFIX, clause) {
        return false;
    }
    for found in pattern(ACTION_BEFORE_BROWSER).find_iter(clause) {
        if !matches(RESEARCH_SUFFIX, &clause[found.end()..]) {
            return true;
        }
    }
    for found in pattern(BROWSER_BEFORE_ACTION).captures_iter(clause) {
        if !matches(RESEARCH_SUFFIX, &found[2]) {
            return true;
        }
    }
    matches(
        r"(?:登录|填写|输入密码|log\s*in|sign\s*in).{0,24}(?:网页|网站|页面|https?://)|(?:网页|网站|页面).{0,24}(?:登录|填写|输入密码)",
        clause,
    )
}

/// Presentation hints select an existing surface; they never authorize a browser or desktop effect.
/// 展示线索仅选择既有界面，不授予浏览器或桌面操作权限。
pub(super) fn browser_presentation(active: &str) -> &'static str {
    let in_workspace = matches(r"右侧|侧栏|工作区|\b(?:sidebar|workspace)\b", active);
    let external = matches(
        r"(?:本机|本地|系统|桌面|外部|我的).{0,12}(?:浏览器|chrome|chromium|edge|firefox)|\b(?:local|system|desktop|external|my)\s+(?:browser|chrome|edge|firefox)\b|(?:启动|运行|launch|start).{0,12}(?:浏览器|chrome|chromium|edge|firefox)",
        active,
    );
    if external && !in_workspace { "application" } else { "workspace" }
}

fn current_browser_intent(message: &str) -> Value {
    let text = browser_instruction(message);
    let projection = analyze_clauses(&text);
    let mut explicit = false;
    let mut local = false;
    let mut excluded = Vec::<String>::new();
    let mut append = |clause: &str| {
        for executable in forbidden_executables(clause) {
            if !excluded.contains(&executable) {
                excluded.push(executable);
            }
        }
    };
    for clause in projection["excludedClauses"].as_array().unwrap() {
        if matches!(
            clause["basis"].as_str(),
            Some("explicit-task-exit" | "superseded-by-task-boundary")
        ) {
            continue;
        }
        let clause = clause["text"].as_str().unwrap();
        if !referenced_instruction(clause, &text) {
            append(clause);
        }
    }
    for clause in pattern(r"[，。；,.!?;！？\n]").split(projection["activeText"].as_str().unwrap())
    {
        if referenced_instruction(clause, &text) {
            continue;
        }
        if matches(LOCAL_BROWSER_FORBIDDEN, clause) {
            append(clause);
            continue;
        }
        if !requests_browser_operation(clause) {
            continue;
        }
        explicit = true;
        let remote = matches(
            r"(?:云端|远程).{0,12}(?:浏览器|chrome|edge)|\b(?:remote|cloud)\s+browser\b",
            clause,
        ) && !matches(
            r"(?:本机|本地|我的|自己的).{0,12}(?:浏览器|chrome|edge)|\blocal\s+browser\b",
            clause,
        );
        local |= !remote;
    }
    // Keep website requests in-app unless a desktop browser is explicit; display hints do not grant authority.
    // 网页默认在软件内打开，明确要求桌面浏览器才用外部应用；展示线索不授予权限。
    let mut intent = json!({"allowLocalBrowser":local,"explicitBrowserTask":explicit,
        "presentationTarget":browser_presentation(projection["activeText"].as_str().unwrap_or(""))});
    if !excluded.is_empty() {
        intent["excludedBrowserExecutables"] = json!(excluded);
    }
    intent
}

fn browser_intent(message: &str, history: &[Value]) -> Value {
    let mut current = current_browser_intent(message);
    let text = browser_instruction(message);
    if analyze_clauses(&text)["boundary"] != "none"
        || !browser_followup(message)
        || matches(BROWSER_NAME, &text)
        || matches(r"右侧|侧栏|工作区|\b(?:sidebar|workspace)\b", &text)
    {
        current["inherited"] = json!(false);
        return current;
    }
    let mut active = None;
    let mut history = history;
    if history.last().and_then(Value::as_str) == Some(message) {
        history = &history[..history.len() - 1];
    }
    for previous in history.iter().filter_map(Value::as_str) {
        let instruction = current_browser_instruction(previous);
        let boundary = analyze_clauses(&instruction)["boundary"] != "none";
        if boundary {
            active = None;
        }
        let intent = current_browser_intent(previous);
        if intent["explicitBrowserTask"] == true
            && (matches(BROWSER_NAME, &instruction)
                || !browser_followup(previous)
                || active.is_none())
        {
            active = Some(intent);
        } else if boundary || !browser_followup(previous) {
            active = None;
        }
    }
    if let Some(mut intent) = active {
        intent["inherited"] = json!(true);
        intent
    } else {
        current["inherited"] = json!(false);
        current
    }
}

fn configured_executable(arguments: &[Value], engine: &str) -> String {
    for (index, option) in arguments.iter().enumerate() {
        let Some(option) = option.as_str() else {
            continue;
        };
        if !matches(
            r"^--(?:executablePath|executable-path|browserExecutablePath|browser)(?:=|$)",
            option,
        ) {
            continue;
        }
        let value = option
            .split_once('=')
            .map(|pair| pair.1)
            .or_else(|| arguments.get(index + 1).and_then(Value::as_str));
        let Some(value) = value else {
            continue;
        };
        let basename = value
            .trim_end_matches(['/', '\\'])
            .rsplit(['/', '\\'])
            .next()
            .unwrap_or("")
            .to_lowercase();
        if is_browser_executable(&basename) {
            return basename;
        }
        if let Some(executable) = executable_name(value) {
            return executable.to_owned();
        }
    }
    if engine == "chrome-devtools" {
        "chrome.exe"
    } else {
        "chromium.exe"
    }
    .to_owned()
}

pub(super) fn is_browser_executable(name: &str) -> bool {
    [
        "chrome.exe",
        "msedge.exe",
        "firefox.exe",
        "chromium.exe",
        "brave.exe",
        "opera.exe",
        "vivaldi.exe",
    ]
    .contains(&name)
}

#[cfg(test)]
mod presentation_tests {
    use super::*;

    #[test]
    fn website_presentation_defaults_to_workspace_and_honors_explicit_desktop_requests() {
        for message in ["在右侧打开B站官网", "打开https://example.org", "不要用本机Chrome，在右侧打开网站", "Open this website in the sidebar"] {
            assert_eq!(current_browser_intent(message)["presentationTarget"], "workspace", "{message}");
        }
        for message in ["用系统浏览器打开网站", "启动本机Chrome", "Open this in the external browser"] {
            assert_eq!(current_browser_intent(message)["presentationTarget"], "application", "{message}");
        }
        let resumed = browser_intent("在右侧打开网站", &[json!("启动本机Chrome")]);
        assert_eq!(resumed["presentationTarget"], "workspace");
        assert_eq!(resumed["inherited"], false);
    }
}

const REQUEST_LEAD: &str = "^(?:(?:请|先|暂时|暂且|现在|目前|我们|我|那|就|并|也|好(?:的)?|这次)\\s*|(?:please|for now|now|currently|we|let['’]s)\\s+)*";
const TASK_EXIT: &str = "^(?:不(?:再)?|别(?:再)?)(?:聊|谈|说|讨论)|^(?:停止(?:讨论|谈论)|搁置|跳过|放下|算了)|^(?:stop|quit)\\s+(?:talking|discussing)|^(?:leave|put|set)\\s+.+\\s+aside\\b|^drop\\s+(?:this|that|the)\\s+(?:topic|subject)\\b|^never\\s+mind\\b";
const TOPIC_SWITCH: &str = "^(?:新任务|新话题|换(?:个|一个)?(?:话题|问题)|(?:聊|谈|说|讨论)(?:另(?:外)?一件事|其他|点别的)|另外一件|另一个问题)|^(?:new\\s+(?:task|topic)|different\\s+topic|(?:switch|change)\\s+(?:the\\s+)?(?:topic|subject)|unrelated\\s+question)\\b";
const CORRECTION: &str = "^(?:不是|并非|更正|纠正|我说的是|而是|改(?:为|成))|^(?:rather\\s+than\\b|i\\s+mean(?:t)?\\b|correction\\b|instead\\b)";
const NEGATED_CLAUSE: &str = "^(?:不是|并非|不要|不必|不用|不使用|无需|不能|不许|别|勿|禁止|避免)|^(?:do\\s+not|don['’]t|never|not|no\\s+need\\s+to|without|rather\\s+than)\\b";
const NEGATED_ACTION: &str = "^(?:不是|并非|不要|不必|不用|无需|不能|不许|别|勿|禁止|避免)\\s*(?:(?:让|要|叫|请)(?:你)?\\s*)?(?:再\\s*)?(?:查|检查|读|看|检索|搜索|分析|讨论|解释|打开|启动|在|用|使用|通过|操作|关闭|刷新|删除|执行|运行)|^不使用|^(?:do\\s+not|don['’]t|never|not|no\\s+need\\s+to|without|rather\\s+than)\\s+(?:open(?:ing)?|launch(?:ing)?|start(?:ing)?|use|using|read(?:ing)?|search(?:ing)?|analy[sz](?:e|ing)|discuss(?:ing)?|operate|close|refresh|delete|execute|run)\\b";
const NEGATED_SOURCE: &str = "^(?:不是|并非|not|rather\\s+than)\\s*(?:(?:a|the|this|that)\\s+)?(?:代码|文档|文件|源码|仓库|知识库|code(?:\\s+repository)?|documents?|docs?|files?|repository|[\\w./\\\\-]+\\.[A-Za-z]{1,8})(?:\\s|$)";
const RETAINED_NEGATION: &str = "^(?:不是|并非)\\s*(?:不|所有|全部|每|任何|一定|总是)|^(?:不要|不能|不许|别|勿|禁止|避免)\\s*(?:忽略|遗漏|省略|丢弃|改动|修改|改变|更改)|^(?:do\\s+not|don['’]t|never|not)\\s+(?:avoid|ignore|overlook|omit|drop|modify|change|all|every|always|necessarily|before|after)\\b";
const CANCELLED_TASK: &str = "^(?:取消|停止|结束)(?:全部|所有|当前|之前)?(?:的)?(?:任务|操作|浏览器操作)(?:了)?$|^(?:(?:浏览器|当前|之前|本次|全部|所有)\\s*)*(?:任务|操作)(?:全部|都)?(?:已)?(?:取消|停止|结束)(?:了)?$|^(?:cancel|stop|end)\\s+(?:(?:all|the|current|previous)\\s+)*(?:browser\\s+)?(?:tasks?|operations?)$";
const FILE_ACTION_LEAD: &str = "^(?:(?:请|你|先|然后|再|依次|直接|帮我)*)(?:读取|读回|回读|读出|打开|查看|列出|写入|写到|写出|保存|创建|新建|追加|复制|移动|重命名|删除)";
const CONTINUATION_REFERENCE: &str = "(?:它|他们|她们|它们)|(?:这个|那个)(?:方法|函数|问题|文件|模块|模型|方案|结果|条件|符号)|这份|那份|这篇|那篇|继续|刚才|上面|\\b(?:it|its|continue|what about|(?:this|that)\\s+(?:method|function|question|file|module|model|result|constraint))\\b";
const EXPLICIT_RETRY: &str = "^(?:(?:请|帮我)\\s*|please\\s+)?(?:再来|重试|再次|再试|尝试|打不开|还是|再(?:查|看)|刷新|现在呢|again\\b|retry\\b|try again\\b|refresh\\b)|^what about now[?.!\\s]*$";
const SUPPLEMENT: &str =
    "^(?:补充|还有|另外[，,]|此外|\\b(?:also|additionally|one\\s+more\\s+thing)\\b)";
const BROWSER_NAME: &str = "(?:浏览器|\\bbrowser\\b|\\b(?:chrome|msedge|edge|firefox|chromium|brave|opera|vivaldi)\\b|\\bgoogle\\b\\s*(?:浏览器|\\bbrowser\\b))";
const ACTION_BEFORE_BROWSER: &str = "(?:打开|开启|启动|操作|控制|显示|切换|激活|截图|截屏|截取|缩放|放大|缩小|调整|最小化|最大化|恢复|刷新|重新加载|关闭|滚动|点击|填写|输入|\\b(?:open|launch|start|operate|control|show|capture|screenshot|focus|activate|resize|zoom|reload|refresh|close|scroll|click|fill|type)\\b)[^，。；,.!?;\\n]{0,24}((?:浏览器|\\bbrowser\\b|\\b(?:chrome|msedge|edge|firefox|chromium|brave|opera|vivaldi)\\b|\\bgoogle\\b\\s*(?:浏览器|\\bbrowser\\b)))";
const BROWSER_BEFORE_ACTION: &str = "((?:浏览器|\\bbrowser\\b|\\b(?:chrome|msedge|edge|firefox|chromium|brave|opera|vivaldi)\\b|\\bgoogle\\b\\s*(?:浏览器|\\bbrowser\\b)))([^，。；,.!?;\\n]{0,16})(?:打开|开启|启动|操作|控制|显示|切换|激活|截图|截屏|截取|缩放|放大|缩小|调整|最小化|最大化|恢复|刷新|重新加载|关闭|滚动|点击|填写|输入|\\b(?:open|launch|start|operate|control|show|capture|screenshot|focus|activate|resize|zoom|reload|refresh|close|scroll|click|fill|type)\\b)";
const USE_BROWSER: &str = "(?:用|使用|通过|\\b(?:use|using|with|via)\\b)\\s*(?:(?:本机|本地|当前|已打开|我的|自己的|云端|远程|the|my|local|current|remote|cloud)\\s*)*((?:浏览器|\\bbrowser\\b|\\b(?:chrome|msedge|edge|firefox|chromium|brave|opera|vivaldi)\\b|\\bgoogle\\b\\s*(?:浏览器|\\bbrowser\\b)))";
const IN_BROWSER: &str = "(?:在|\\bin\\b)\\s*(?:(?:本机|本地|当前|已打开|我的|自己的|云端|远程|the|my|local|current|remote|cloud)\\s*)*((?:浏览器|\\bbrowser\\b|\\b(?:chrome|msedge|edge|firefox|chromium|brave|opera|vivaldi)\\b|\\bgoogle\\b\\s*(?:浏览器|\\bbrowser\\b)))";
const READ_BROWSER: &str = "(?:查看|读取|\\b(?:inspect|read)\\b)\\s*((?:(?:本机|本地|当前|已打开|我的|自己的|云端|远程|the|my|local|current|remote|cloud)\\s*)*)((?:浏览器|\\bbrowser\\b|\\b(?:chrome|msedge|edge|firefox|chromium|brave|opera|vivaldi)\\b|\\bgoogle\\b\\s*(?:浏览器|\\bbrowser\\b)))";
const BROWSER_SEARCH: &str = "(?:让|请)\\s*(?:(?:本机|本地|当前|已打开|我的|自己的|云端|远程|the|my|local|current|remote|cloud)\\s*)*((?:浏览器|\\bbrowser\\b|\\b(?:chrome|msedge|edge|firefox|chromium|brave|opera|vivaldi)\\b|\\bgoogle\\b\\s*(?:浏览器|\\bbrowser\\b)))[^，。；,.!?;\\n]{0,8}(?:搜索|查询|读取|查看)";
const RESEARCH_SUFFIX: &str = "^\\s*(?:(?:浏览器|browser)\\s*)?(?:的\\s*)?(?:最新(?:版本|新闻|消息)|版本|文档|官网|发行|发布说明|相关新闻|latest\\b|version\\b|docs?\\b|documentation\\b|release\\b|news\\b)";
const RESEARCH_PREFIX: &str = "^\\s*(?:请\\s*)?(?:搜索|查询|查找|检索|查|了解|研究|查看|读取|\\b(?:search|find|read|inspect|research)\\b|look\\s+up)";
const INFORMATIONAL_QUESTION: &str = "怎么|如何|怎样|\\bhow\\b";
const EXPLICIT_EXECUTION_REQUEST: &str = "(?:帮我|替我|请你)\\s*(?:实际|直接)?\\s*(?:用|使用|操作|执行|演示|打开|启动|截图|截屏)|(?:请\\s*)?(?:实际|直接)\\s*(?:演示|操作|执行|用|使用)|\\b(?:please\\s+)?(?:actually\\s+demonstrate|demonstrate|help\\s+me\\s+use|do\\s+this\\s+for\\s+me)\\b";
const LOCAL_BROWSER_FORBIDDEN: &str = "(?:不要|别|不必|禁止|不能|不许|不允许|不希望|避免|无需|do not|don['’]t|never|without)[^，。；,.!?;\\n]{0,24}?(?:打开|开启|启动|操作|使用|用|开|open|launch|start|use|operate|control)(?<objectPrefix>[^，。；,.!?;\\n]{0,16}?)(?<browserObject>(?:浏览器|\\bbrowser\\b|\\b(?:chrome|msedge|edge|firefox|chromium|brave|opera|vivaldi)\\b|\\bgoogle\\b\\s*(?:浏览器|\\bbrowser\\b)))|(?:不用|不使用)(?<useObjectPrefix>[^，。；,.!?;\\n]{0,16}?)(?<useBrowserObject>(?:浏览器|\\bbrowser\\b|\\b(?:chrome|msedge|edge|firefox|chromium|brave|opera|vivaldi)\\b|\\bgoogle\\b\\s*(?:浏览器|\\bbrowser\\b)))";
const WINDOW_OBJECT: &str = "^\\s*(?:(?:浏览器|browser)\\s*)?(?:(?:的|['’]s)\\s*)?(?:(?:本机|本地|当前|已有|已打开|这个|该|新|无痕|隐私|的|a|an|the|new|current|existing|private)\\s*)*(?:窗口|标签(?:页)?|页面|网页|\\b(?:windows?|tabs?|pages?)\\b)";
const COORDINATED_BROWSER_OBJECT: &str = "^\\s*(?:和|与|及|以及|或(?:者)?|、|\\/|&|\\b(?:and|or)\\b)\\s*(?:(?:谷歌|微软|Microsoft|Google|Mozilla|a|an|the|local)\\s*)*(?<browserObject>(?:浏览器|\\bbrowser\\b|\\b(?:chrome|msedge|edge|firefox|chromium|brave|opera|vivaldi)\\b|\\bgoogle\\b\\s*(?:浏览器|\\bbrowser\\b)))";

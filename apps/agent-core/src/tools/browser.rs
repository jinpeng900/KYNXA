use serde_json::{Value, json};

use super::intent::{matches, pattern};

fn error(message: &str, code: &str) -> Value {
    json!({"error":{"message":message,"code":code}})
}

fn operation(arguments: &Value) -> Value {
    let descriptor = &arguments["descriptor"];
    let connection = &arguments["connection"];
    let Some(engine) = connection["engine"].as_str() else {
        return Value::Null;
    };
    if descriptor["operation"] != "tools/call" {
        return Value::Null;
    }
    let name = descriptor["toolName"].as_str().unwrap_or("");
    let read_only = if engine == "chrome-devtools" {
        [
            "list_pages",
            "take_snapshot",
            "take_screenshot",
            "list_console_messages",
            "get_console_message",
            "list_network_requests",
            "get_network_request",
        ]
        .contains(&name)
    } else {
        [
            "browser_snapshot",
            "browser_take_screenshot",
            "browser_console_messages",
            "browser_network_requests",
        ]
        .contains(&name)
    };
    json!({"engine":engine,"mode":connection["mode"].as_str().unwrap_or("custom-browser"),"headless":connection["headless"] == true,
        "navigation":(["new_page","navigate_page","browser_navigate","browser_navigate_back","browser_navigate_forward"].contains(&name)),
        "readOnly":read_only || name == "browser_tabs" && arguments["action"] == "list"})
}

fn prepare_options(arguments: &Value) -> Value {
    let foreground = arguments["allowForeground"] != false;
    let mut background = arguments["background"] != false;
    let prepared = &arguments["args"];
    let properties = &arguments["properties"];
    let operation = &arguments["operation"];
    let name = arguments["toolName"].as_str().unwrap_or("");
    if prepared["background"] == false || prepared["bringToFront"] == true {
        background = false;
    }
    if !foreground && !background {
        return error(
            "当前任务要求后台浏览，不能激活浏览器；请保持后台或由用户明确要求前台。",
            "BROWSER_FOREGROUND_FORBIDDEN",
        );
    }
    let mut patch = json!({});
    let mut page_id = false;
    if operation["engine"] == "chrome-devtools" {
        if background
            && (name == "new_page" && properties["background"] != true
                || name == "select_page" && properties["bringToFront"] != true)
        {
            if foreground {
                background = false;
            } else {
                return error(
                    "此连接未声明后台标签页参数，不能保证不激活浏览器。",
                    "BROWSER_BACKGROUND_UNSUPPORTED",
                );
            }
        }
        if name == "new_page"
            && properties["background"] == true
            && prepared["background"].is_null()
        {
            patch["background"] = json!(background);
        }
        if name == "select_page"
            && properties["bringToFront"] == true
            && prepared["bringToFront"].is_null()
        {
            patch["bringToFront"] = json!(!background);
        }
        page_id = properties["pageId"] == true
            && prepared["pageId"].is_null()
            && !arguments["selectedTabId"].is_null();
    }
    if background
        && operation["engine"] == "playwright"
        && operation["headless"] != true
        && name == "browser_tabs"
        && ["select", "new"]
            .iter()
            .any(|action| prepared["action"] == *action)
    {
        if foreground {
            background = false;
        } else {
            return error(
                "此 Playwright 标签页操作没有后台参数。请使用支持后台标签页的连接。",
                "BROWSER_BACKGROUND_UNSUPPORTED",
            );
        }
    }
    json!({"background":background,"patch":patch,"injectPageId":page_id,"requiresScriptCheck":!foreground && ["evaluate_script","browser_evaluate","browser_run_code"].contains(&name)})
}

// Only the current tab and matching reference membership cross the bridge, never the full tab archive.
// 只传当前标签页和相关引用的成员事实，不复制整个标签页档案；模型说明不能制造快照身份。
fn target_guard(arguments: &Value) -> Value {
    let selecting = arguments["selecting"] == true;
    if !selecting
        && arguments["pageIdDeclared"] != true
        && !arguments["sessionTabId"].is_null()
        && !arguments["connectionTabId"].is_null()
        && arguments["sessionTabId"] != arguments["connectionTabId"]
    {
        return error(
            "浏览器标签页已由另一任务切换，请先重新选择并读取当前页。",
            "BROWSER_TAB_CHANGED",
        );
    }
    if arguments["referenceCount"].as_u64().unwrap_or(0) > 0 {
        if arguments["tabId"].is_null() {
            return error(
                "当前快照尚无可验证的标签页 ID，请先列出标签页，再读取该页的新快照。",
                "BROWSER_TAB_ID_REQUIRED",
            );
        }
        if arguments["hasSnapshot"] != true
            || arguments["snapshotSessionId"] != arguments["sessionId"]
            || arguments["missingReferenceCount"].as_u64().unwrap_or(0) > 0
        {
            return error(
                "页面元素引用已过期或不属于当前聊天；请读取此标签页的新快照。",
                "BROWSER_STALE_REFERENCE",
            );
        }
    }
    if arguments["readOnly"] != true && arguments["needsObservation"] == true {
        return error(
            "上次浏览器操作结果尚未确认，请先读取当前页面的新快照。",
            "BROWSER_OBSERVATION_REQUIRED",
        );
    }
    Value::Null
}

fn timing(arguments: &Value) -> Value {
    let navigation = arguments["navigation"] == true;
    let inner = if navigation { 60000 } else { 15000 };
    let numeric = arguments["timeout"].as_f64();
    let integer = numeric.is_some_and(|number| number.is_finite() && number.fract() == 0.0);
    let patch = if arguments["timeoutDeclared"] == true && (!integer || numeric == Some(0.0)) {
        json!({"timeout":inner})
    } else {
        json!({})
    };
    let configured = if integer && numeric.unwrap_or(0.0) > 0.0 {
        numeric.unwrap()
    } else {
        inner as f64
    };
    if configured > 120000.0 {
        return error(
            "浏览器动作期限不能超过 120 秒；长任务应拆成可验证步骤。",
            "BROWSER_TIMEOUT_OUT_OF_RANGE",
        );
    }
    let timeout = (configured as u64 + 10000).max(if navigation { 70000 } else { 30000 });
    json!({"patch":patch,"timeoutMs":timeout})
}

fn references(input: &Value, depth: usize, output: &mut Vec<String>) {
    if depth > 8 {
        return;
    }
    let mut visit = |key: &str, child: &Value| {
        if let Some(value) = child.as_str() {
            if matches(r"(?-i:^(?:uid|\w+_uid|ref|\w+Ref)$)", key)
                || key == "target"
                    && matches(r"(?-i:^(?:[ef][0-9]+(?:e[0-9]+)?|[0-9]+_[0-9]+)$)", value)
            {
                output.push(value.to_owned());
            }
        } else {
            references(child, depth + 1, output);
        }
    };
    match input {
        Value::Object(object) => {
            for (key, child) in object {
                visit(key, child);
            }
        }
        Value::Array(items) => {
            for (index, child) in items.iter().enumerate() {
                visit(&index.to_string(), child);
            }
        }
        _ => {}
    }
}

fn receipt_evidence(text: &str) -> Value {
    let mut tabs = Vec::new();
    for found in pattern(r"(?m-i:^([0-9]+):\s+([^\r\n]+)$)").captures_iter(text) {
        let label = &found[2];
        let selected = matches(r"(?-i:\s+\[selected\]$)", label);
        let label = pattern(r"(?-i:\s+\[selected\]$)")
            .replace(label, "")
            .to_string();
        let url = pattern(r"(?-i:\((https?://.*)\)$)")
            .captures(&label)
            .map(|found| found[1].to_owned())
            .unwrap_or(label);
        tabs.push(json!({"tabId":found[1].to_owned(),"url":url,"selected":selected}));
    }
    for found in pattern(r"(?m-i:^-\s+([0-9]+):\s+(\(current\)\s+)?\[[^\r\n]*?\]\(([^\r\n]*)\))")
        .captures_iter(text)
    {
        tabs.push(json!({"tabId":found[1].to_owned(),"url":found[3].to_owned(),"selected":found.get(2).is_some()}));
    }
    let mut refs = Vec::<String>::new();
    for found in pattern(r"(?-i:\buid=([^\s]+)|\[ref=([^\]\s]+)\])").captures_iter(text) {
        let value = found.get(1).or_else(|| found.get(2)).unwrap().as_str();
        if refs.len() < 4096 && !refs.iter().any(|reference| reference == value) {
            refs.push(value.to_owned());
        }
    }
    let page = pattern(r"(?m-i:^- Page URL:\s*(\S+))")
        .captures(text)
        .map(|found| found[1].to_owned())
        .or_else(|| {
            pattern(r#"(?-i:RootWebArea[^\r\n]*\burl="([^"]+)")"#)
                .captures(text)
                .map(|found| found[1].to_owned())
        });
    json!({"tabs":tabs,"refs":refs,"pageUrl":page})
}

fn approval_guard(arguments: &Value) -> Value {
    let expected = &arguments["expected"];
    if expected.is_null() {
        return Value::Null;
    }
    let current = &arguments["current"];
    if current.is_null()
        || ["connectionId", "sessionId", "toolName"]
            .iter()
            .any(|key| current[*key] != expected[*key])
    {
        return error(
            "浏览器审批身份已变化，请重新准备调用和审批。",
            "BROWSER_APPROVAL_CHANGED",
        );
    }
    if expected["targetsPage"] == true && current["tabId"] != expected["tabId"] {
        return error(
            "审批期间浏览器目标标签页已变化，请读取目标页并重新审批。",
            "BROWSER_TAB_CHANGED",
        );
    }
    if !expected["snapshotGeneration"].is_null()
        && current["snapshotGeneration"] != expected["snapshotGeneration"]
    {
        return error(
            "审批期间页面快照已变化，请读取新快照并重新审批。",
            "BROWSER_STALE_REFERENCE",
        );
    }
    Value::Null
}

fn recovery_allowed(arguments: &Value) -> bool {
    ["BROWSER_TAB_ID_REQUIRED", "BROWSER_STALE_REFERENCE"]
        .iter()
        .any(|code| arguments["code"] == *code)
        && arguments["hasOperation"] == true
        && (arguments["requestedTabId"].is_null()
            || arguments["sessionTabId"] == arguments["requestedTabId"])
        && (arguments["pageIdDeclared"] == true
            || arguments["sessionTabId"].is_null()
            || arguments["connectionTabId"].is_null()
            || arguments["sessionTabId"] == arguments["connectionTabId"])
        && (!arguments["requestedTabId"].is_null() || arguments["hasLastUrl"] == true)
}

pub(super) fn calculate(operation_name: &str, arguments: &Value) -> Result<Value, String> {
    Ok(match operation_name {
        "tools.browserOperation" => operation(arguments),
        "tools.browserPrepareOptions" => prepare_options(arguments),
        "tools.browserTargetGuard" => target_guard(arguments),
        "tools.browserTiming" => timing(arguments),
        "tools.browserReferences" => { let mut refs=Vec::new(); references(&arguments["args"],0,&mut refs); json!(refs) },
        "tools.browserReceiptEvidence" => receipt_evidence(arguments["text"].as_str().unwrap_or("")),
        "tools.browserApprovalGuard" => approval_guard(arguments),
        "tools.browserRecoveryAllowed" => json!(recovery_allowed(arguments)),
        "tools.browserUrlAllowed" => json!(arguments["protocol"].as_str().is_some_and(|value| ["http:","https:","about:"].contains(&value))),
        "tools.browserPrivateQueryNames" => json!(arguments["names"].as_array().map(Vec::as_slice).unwrap_or(&[]).iter().filter(|name| name.as_str().is_some_and(|name| matches(r"token|secret|password|credential|api[-_]?key|authorization|signature|^code$|^key$",name))).cloned().collect::<Vec<_>>()),
        _ => return Err("AGENT_CORE_UNKNOWN_OPERATION".to_owned()),
    })
}

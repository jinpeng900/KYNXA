use serde_json::{Value, json};

// Descriptor annotations and model explanations never grant execution authority.
// 描述符注解和模型理由不授予执行权限；审批事件的身份与生命周期仍由宿主适配器管理。
pub(super) fn calculate(operation: &str, arguments: &Value) -> Result<Value, String> {
    match operation {
        "tools.nativeAvailability" => return Ok(native_availability(arguments)),
        "tools.desktopObservation" => {
            return Ok(json!(
                DESKTOP_OBSERVATIONS.contains(&arguments["name"].as_str().unwrap_or(""))
            ));
        }
        "tools.observationFailure" => return Ok(json!(observation_failure(arguments))),
        "tools.canParallel" => return Ok(json!(can_parallel(arguments))),
        "tools.desktopLaunchOptions" => return Ok(desktop_launch_options(arguments)),
        "tools.needsApproval" => {}
        _ => return Err("AGENT_CORE_UNKNOWN_OPERATION".to_owned()),
    }
    let mode = arguments["permissionMode"].as_str().unwrap_or("");
    let name = arguments["name"].as_str().unwrap_or("");
    let options = &arguments["options"];
    let approved_without_prompt = if mode == "full" {
        true
    } else if options["sensitiveRead"] == true {
        false
    } else if ["web.fetch", "computer.apps", "computer.windows"].contains(&name) {
        true
    } else if options["outsideWorkspace"] == true {
        false
    } else if READ_TOOLS.contains(&name) || BROWSER_OBSERVATIONS.contains(&name) {
        true
    } else {
        mode == "smart"
            && (REVERSIBLE_TOOLS.contains(&name)
                || ["terminal.run", "skill.run"].contains(&name)
                    && options["verifiedSandbox"] == true)
    };
    Ok(json!(!approved_without_prompt))
}

/// Read-only recovery uses declared builtin semantics, never name suffixes or model/MCP annotations.
/// 只读恢复依据内置工具的明确业务语义；不依据名称后缀、模型说明或外部 MCP 注解放行。
pub fn is_read_only(name: &str) -> bool {
    DESKTOP_OBSERVATIONS.contains(&name)
        || PARALLEL_READS.contains(&name)
        || READ_TOOLS.contains(&name) && !["memory.propose", "tool.load"].contains(&name)
        || BROWSER_OBSERVATIONS.contains(&name)
}

#[cfg(test)]
mod recovery_tests {
    use super::*;

    #[test]
    fn recovery_classification_uses_builtin_semantics_without_suffix_or_annotation_guesses() {
        for name in [
            "browser.tabs",
            "browser.snapshot",
            "browser.console",
            "browser.network",
            "computer.apps",
            "computer.windows",
            "computer.screenshot",
            "web.fetch",
            "knowledge.assess",
            "memory.read",
            "terminal.host.read",
        ] {
            assert!(is_read_only(name), "{name}");
        }
        for name in [
            "browser.navigate",
            "browser.upload",
            "browser.dialog",
            "browser.select",
            "computer.launch",
            "computer.type",
            "filesystem.write",
            "memory.propose",
            "tool.load",
            "mcp.untrusted.read",
            "missing.read",
        ] {
            assert!(!is_read_only(name), "{name}");
        }
    }
}

fn supports_skill(capabilities: &Value) -> bool {
    let proof = &capabilities["skillExecution"];
    proof["manifestVersion"] == 1
        && proof["readOnlyPackage"] == true
        && proof["hashChecked"] == true
}

fn native_availability(arguments: &Value) -> Value {
    let context = &arguments["context"];
    if !context.is_object() {
        return json!({});
    }
    let name = arguments["name"].as_str().unwrap_or("");
    let (available, code) = if let Some(operation) = name.strip_prefix("computer.") {
        let capabilities = &context["desktopCapabilities"];
        (
            capabilities["available"] == true
                && capabilities["boundary"] == "host-desktop"
                && capabilities["operations"]
                    .as_array()
                    .is_some_and(|values| values.iter().any(|value| value == operation)),
            "DESKTOP_UNAVAILABLE",
        )
    } else if name.starts_with("terminal.host.") {
        let capabilities = &context["hostTerminalCapabilities"];
        (
            capabilities["available"] == true
                && capabilities["boundary"] == "host-terminal"
                && (name == "terminal.host.run" || capabilities["backgroundJobs"] == true),
            "HOST_TERMINAL_UNAVAILABLE",
        )
    } else if ["terminal.run", "skill.run"].contains(&name) {
        if context["workspaceDiagnostic"]
            .as_str()
            .is_some_and(|value| !value.is_empty())
        {
            return json!({"available":false,"unavailableCode":"WORKSPACE_UNAVAILABLE"});
        }
        let sandbox = &context["sandboxCapabilities"];
        (
            sandbox["available"] == true
                && sandbox["sandbox"] == "appcontainer"
                && sandbox["failClosed"] == true
                && sandbox["checksChildToken"] == true
                && (name != "skill.run" || supports_skill(sandbox)),
            "SANDBOX_UNAVAILABLE",
        )
    } else {
        return json!({});
    };
    if available {
        json!({"available":true})
    } else {
        json!({"available":false,"unavailableCode":code})
    }
}

fn observation_failure(arguments: &Value) -> bool {
    let result = &arguments["result"];
    let name = arguments["name"].as_str().unwrap_or("");
    if result["isError"] != true
        || result["status"] == "unknown"
        || result["code"] == "TOOL_CANCELLED"
    {
        return false;
    }
    let code = result["code"].as_str().unwrap_or("");
    if DESKTOP_OBSERVATIONS.contains(&name) {
        return [
            "DESKTOP_TIMEOUT",
            "DESKTOP_TIMED_OUT",
            "DESKTOP_READ_TIMEOUT",
            "DESKTOP_NOT_RESPONDING",
            "DESKTOP_WINDOW_UNAVAILABLE",
            "DESKTOP_TARGET_CHANGED",
            "DESKTOP_CAPTURE_FAILED",
        ]
        .contains(&code);
    }
    name.starts_with("mcp.")
        && result["browser"]["readOnly"] == true
        && [
            "MCP_TIMEOUT",
            "MCP_CONNECTION_CLOSED",
            "MCP_CONNECTION_LOST",
            "MCP_TOOL_FAILED",
        ]
        .contains(&code)
}

fn can_parallel(arguments: &Value) -> bool {
    let Some(name) = arguments["name"].as_str() else {
        return false;
    };
    if PARALLEL_READS.contains(&name) {
        return true;
    }
    let operation = regex::Regex::new(r"^mcp\.official-(exa|fetch|brave-search)(?:-([1-9][0-9]*))?\.(web_search_exa|fetch|brave_web_search)$").unwrap();
    let effective = operation
        .captures(name)
        .filter(|captures| captures.get(2).is_none_or(|number| number.as_str() != "1"))
        .map(|captures| format!("mcp.{}.{}", &captures[1], &captures[3]))
        .unwrap_or_else(|| name.to_owned());
    if [
        "mcp.exa.web_search_exa",
        "mcp.brave.brave_web_search",
        "mcp.brave-search.brave_web_search",
    ]
    .contains(&effective.as_str())
    {
        return true;
    }
    effective == "mcp.fetch.fetch" && arguments["publicUrl"] == true
}

fn desktop_launch_options(arguments: &Value) -> Value {
    let background = arguments["background"]
        .as_bool()
        .unwrap_or(arguments["allowForeground"] != true);
    let basename = arguments["basename"].as_str().unwrap_or("");
    let mut result = json!({"background":background});
    if background && ["chrome.exe", "msedge.exe"].contains(&basename) {
        let mut args = arguments["args"].as_array().cloned().unwrap_or_default();
        if !args.iter().filter_map(Value::as_str).any(|value| {
            value == BACKGROUND_BROWSER_FLAG
                || value.starts_with(&format!("{BACKGROUND_BROWSER_FLAG}="))
        }) {
            args.push(json!(BACKGROUND_BROWSER_FLAG));
        }
        result["args"] = json!(args);
    }
    result
}

const BACKGROUND_BROWSER_FLAG: &str = "--disable-backgrounding-occluded-windows";
const DESKTOP_OBSERVATIONS: &[&str] = &[
    "computer.apps",
    "computer.windows",
    "computer.read",
    "computer.screenshot",
];
const BROWSER_OBSERVATIONS: &[&str] = &[
    "browser.tabs",
    "browser.snapshot",
    "browser.screenshot",
    "browser.console",
    "browser.network",
    "browser.wait",
];
const PARALLEL_READS: &[&str] = &[
    "filesystem.read",
    "filesystem.list",
    "filesystem.search",
    "filesystem.stat",
    "web.fetch",
    "conversation.history.read",
    "conversation.history.search",
    "tool.result.read",
    "knowledge.search",
    "knowledge.read",
    "knowledge.relations",
    "knowledge.experience",
    "memory.read",
];

const READ_TOOLS: &[&str] = &[
    "filesystem.list",
    "filesystem.read",
    "filesystem.search",
    "filesystem.stat",
    "skill.list",
    "skill.read",
    "tool.search",
    "tool.load",
    "tool.result.read",
    "skill.resource.read",
    "skill.inspect",
    "skill.check",
    "conversation.history.search",
    "conversation.history.read",
    "knowledge.search",
    "knowledge.read",
    "knowledge.relations",
    "knowledge.plan",
    "knowledge.assess",
    "knowledge.experience",
    "memory.read",
    "memory.propose",
    "web.search",
    "terminal.host.read",
];
const REVERSIBLE_TOOLS: &[&str] = &[
    "filesystem.write",
    "filesystem.edit",
    "filesystem.mkdir",
    "work.folder.bind",
];

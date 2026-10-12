use std::collections::{HashMap, HashSet};

use base64::{Engine, engine::general_purpose::STANDARD};
use serde_json::{Value, json};

use super::intent::{js_trim, matches};
use super::local_files::{absolute_path, inspect, path_text, within};
use super::service_error::{ServiceError, ServiceResult};
use crate::models::protocol::{array, string, truthy};

fn failure(code: &str, message: &str) -> ServiceError {
    ServiceError::new(code, message, 400)
}
fn integer(value: &Value, minimum: u64, maximum: u64) -> bool {
    value
        .as_u64()
        .is_some_and(|value| value >= minimum && value <= maximum && value <= 9_007_199_254_740_991)
}
fn host_envelope(value: &Value) -> bool {
    [Some(1), Some(2)].contains(&value["protocolVersion"].as_u64())
        && value["boundary"] == "host-terminal"
}
fn desktop_envelope(value: &Value) -> bool {
    value["protocolVersion"] == 1 && value["boundary"] == "host-desktop"
}
const ACTIONS: &[&str] = &[
    "windows",
    "apps",
    "screenshot",
    "read",
    "launch",
    "window",
    "activate",
    "move",
    "click",
    "scroll",
    "drag",
    "type",
    "key",
];

fn visible_console(value: &Value, hold: u64) -> bool {
    value["protocolVersion"] == 2
        && value["visibleRequested"] == true
        && value["windowObserved"] == true
        && matches(r"^\d+$", string(&value["windowId"]))
        && value["windowId"] != "0"
        && integer(&value["windowProcessId"], 1, 9_007_199_254_740_991)
        && value["consoleInput"] == "console"
        && value["consoleOutput"] == "console"
        && value["outputCapture"] == "console-screen"
        && value["commandCompleted"] == true
        && value["exitCodeObserved"] == true
        && value["outcome"] == "completed"
        && value["consoleText"].is_string()
        && value["consoleSnapshotAvailable"].is_boolean()
        && integer(&value["displayHoldMs"], 0, hold)
        && value["stdout"] == ""
        && value["stderr"] == ""
}

/// Native runners own admission, request and receipt rules; actual ToolHost IPC stays with runtime handles.
/// 原生执行器拥有准入、请求与回执规则；实际 ToolHost 通信保留在运行时句柄边界。
#[derive(Default)]
pub struct RunnerState {
    pending: usize,
    verified_sandbox: bool,
    verified_skills: bool,
    stages: HashSet<String>,
    excluded_roots: Vec<String>,
    workspace_home: Option<String>,
    protocol: ProtocolState,
}

impl RunnerState {
    pub fn apply(&mut self, operation: &str, arguments: &Value) -> ServiceResult<Value> {
        match operation {
            "runner.abort" => Ok(
                json!({"name":"AbortError","code":"ABORT_ERR","message":"Sandbox execution was cancelled."}),
            ),
            "runner.invalidSkillReceipt" => Err(failure(
                "SANDBOX_INVALID_RESULT",
                "The native helper did not prove execution of the selected skill package.",
            )),
            "runner.capabilitiesFailure" => Ok(match string(&arguments["kind"]) {
                "desktop" => {
                    json!({"protocolVersion":1,"boundary":"host-desktop","available":false,"interactiveWindows":false,"operations":[],"reason":arguments["message"]})
                }
                "host" => {
                    json!({"protocolVersion":1,"boundary":"host-terminal","available":false,"shells":[],"sandbox":false,"reason":arguments["message"]})
                }
                _ => {
                    json!({"protocolVersion":1,"available":false,"sandbox":"appcontainer","commands":[],"network":false,"workspaceCopy":true,"failClosed":true,"checksChildToken":true,"reason":arguments["message"]})
                }
            }),
            "runner.capabilitiesTimeout" => Ok(json!(10000)),
            "runner.closedMetadata" => {
                let capabilities = self.apply("runner.capabilitiesFailure", arguments)?;
                Ok(json!({"capabilities":capabilities,"pending":self.pending}))
            }
            "runner.transportFailure" => Ok(
                json!({"code":match string(&arguments["kind"]){"unavailable"=>json!("SANDBOX_UNAVAILABLE"),_=>arguments.get("code").cloned().unwrap_or(json!("SANDBOX_START_FAILED"))},"message":arguments["message"]}),
            ),
            "runner.toolHostPlatform" => {
                if arguments["platform"] != "win32" {
                    return Err(failure(
                        string(&arguments["unavailableCode"]),
                        "The native tool host requires Windows.",
                    ));
                }
                Ok(Value::Null)
            }
            "runner.toolHostCandidates" => {
                for (index, path) in array(&arguments["candidates"]).iter().enumerate() {
                    let path = absolute_path(string(path));
                    let metadata = match std::fs::symlink_metadata(&path) {
                        Ok(metadata) => metadata,
                        Err(error)
                            if matches!(
                                error.kind(),
                                std::io::ErrorKind::NotFound | std::io::ErrorKind::NotADirectory
                            ) =>
                        {
                            continue;
                        }
                        Err(error) => return Err(ServiceError::io(error)),
                    };
                    if !metadata.is_file() || super::local_files::is_link(&metadata) {
                        continue;
                    }
                    match std::fs::File::open(path) {
                        Ok(_) => return Ok(json!(index)),
                        Err(error)
                            if matches!(
                                error.kind(),
                                std::io::ErrorKind::NotFound | std::io::ErrorKind::NotADirectory
                            ) =>
                        {
                            continue;
                        }
                        Err(error) => return Err(ServiceError::io(error)),
                    }
                }
                Err(failure(
                    string(&arguments["unavailableCode"]),
                    "The native KYNXA.ToolHost executable has not been built or packaged.",
                ))
            }
            "runner.resourcePlan" => {
                let kind = string(&arguments["kind"]);
                let request = &arguments["request"];
                let mut requirements = json!({"cpuThreads":if kind=="desktop"{1}else{2},"memoryBytes":if kind=="desktop"{if request["action"]=="screenshot"{192*1024*1024}else{32*1024*1024}}else{64*1024*1024}});
                if kind == "host" {
                    requirements["kind"] = json!(if request["operation"] == "host_terminal_job" {
                        "background"
                    } else {
                        "foreground"
                    });
                }
                Ok(
                    json!({"required":(!["desktop_capabilities","host_terminal_capabilities","capabilities"].contains(&string(&request["operation"]))),"requirements":requirements,"maximumWaitMs":request["timeoutMs"].as_u64().unwrap_or(0)+if kind=="sandbox"{30000}else{10000}}),
                )
            }
            "runner.hostBegin" => {
                if self.pending >= 8 {
                    return Err(ServiceError::new(
                        "HOST_TERMINAL_CAPACITY",
                        "本机终端并发已达上限，请稍后重试。",
                        429,
                    ));
                }
                self.pending += 1;
                Ok(json!(self.pending))
            }
            "runner.hostEnd" => {
                self.pending = self.pending.saturating_sub(1);
                Ok(json!(self.pending))
            }
            "runner.pending" => Ok(json!(self.pending)),
            "runner.hostCapabilities" => {
                let value = &arguments["value"];
                if !host_envelope(value)
                    || value["available"] != true
                    || value["sandbox"] != false
                    || value["processTreeBounded"] != true
                    || !value["shells"].is_array()
                    || array(&value["shells"]).is_empty()
                    || array(&value["shells"])
                        .iter()
                        .any(|shell| !["cmd", "powershell"].contains(&string(shell)))
                {
                    return Err(ServiceError::new(
                        "HOST_TERMINAL_UNAVAILABLE",
                        "原生助手没有可用的本机终端能力。",
                        503,
                    ));
                }
                Ok(Value::Null)
            }
            "runner.hostRequest" => {
                let input = &arguments["input"];
                let background = input["backgroundJob"] == true;
                let visible = input["visible"] == true;
                if !["cmd", "powershell"].contains(&string(&input["shell"]))
                    || !input["script"].is_string()
                    || js_trim(string(&input["script"])).is_empty()
                    || string(&input["script"]).encode_utf16().count() > 16384
                    || string(&input["script"]).contains('\0')
                    || !integer(
                        &input["timeoutMs"],
                        100,
                        if background { 21600000 } else { 120000 },
                    )
                {
                    return Err(failure(
                        "HOST_TERMINAL_INVALID_REQUEST",
                        "本机终端的命令或超时参数无效。",
                    ));
                }
                if !input["visible"].is_boolean()
                    || !input["backgroundJob"].is_boolean()
                    || background && visible
                    || input
                        .get("keepOpenMs")
                        .is_some_and(|value| !visible || !integer(value, 0, 30000))
                {
                    return Err(failure(
                        "HOST_TERMINAL_INVALID_REQUEST",
                        "本机终端窗口参数无效。",
                    ));
                }
                let cwd = input["cwd"].as_str().unwrap_or("");
                if !input["cwd"].is_string()
                    || arguments["cwdAbsolute"] != true
                    || cwd.starts_with(r"\\")
                    || cwd.contains(['\0', '\r', '\n'])
                {
                    return Err(failure(
                        "HOST_TERMINAL_INVALID_WORKSPACE",
                        "本机终端需要有效的绝对本地目录。",
                    ));
                }
                Ok(
                    json!({"holdMs":input["keepOpenMs"].as_u64().unwrap_or(if visible{5000}else{0}),"operation":if background{"host_terminal_job"}else if visible{"host_terminal_visible"}else{"host_terminal"},"maximumWaitMs":input["timeoutMs"].as_u64().unwrap()+10000}),
                )
            }
            "runner.hostPath" => {
                let binding = &arguments["binding"];
                if binding["path"] != arguments["cwd"]
                    && binding["requestedPath"] != arguments["cwd"]
                {
                    return Err(failure(
                        "HOST_TERMINAL_INVALID_WORKSPACE",
                        "本机终端工作目录身份不匹配。",
                    ));
                }
                Ok(Value::Null)
            }
            "runner.hostDirectory" => {
                if arguments["directory"] != true {
                    return Err(failure(
                        "HOST_TERMINAL_INVALID_WORKSPACE",
                        "本机终端工作目录不存在。",
                    ));
                }
                Ok(Value::Null)
            }
            "runner.hostRequiredCapabilities" => {
                let value = &arguments["value"];
                if arguments["visible"] == true
                    && (value["available"] != true
                        || value["protocolVersion"] != 2
                        || value["visibleTerminal"] != true)
                {
                    return Err(ServiceError::new(
                        "HOST_TERMINAL_VISIBLE_UNAVAILABLE",
                        "当前原生助手不支持可见终端，请更新后重试。",
                        503,
                    ));
                }
                if arguments["backgroundJob"] == true
                    && (value["available"] != true
                        || value["backgroundJobs"] != true
                        || !integer(&value["maximumJobTimeoutMs"], 100, 21600000)
                        || arguments["timeoutMs"].as_u64() > value["maximumJobTimeoutMs"].as_u64())
                {
                    return Err(ServiceError::new(
                        "HOST_TERMINAL_BACKGROUND_UNAVAILABLE",
                        "当前原生助手不支持受控后台终端任务，请更新后重试。",
                        503,
                    ));
                }
                Ok(Value::Null)
            }
            "runner.hostResult" => {
                let value = &arguments["value"];
                if !host_envelope(value) {
                    return Err(ServiceError::new(
                        "HOST_TERMINAL_INVALID_RESULT",
                        "本机终端回执无效。",
                        502,
                    ));
                }
                if value["completed"] == true {
                    let identity = value["shell"] == arguments["shell"]
                        && value["cwd"] == arguments["cwd"]
                        && value["exitCode"].as_i64().is_some()
                        && value["stdout"].is_string()
                        && value["stderr"].is_string()
                        && value["activeProcessesAfterExit"] == 0
                        && !truthy(&value["error"]);
                    let visible = arguments["visible"] == true;
                    let window = if visible {
                        visible_console(value, arguments["holdMs"].as_u64().unwrap_or(0))
                    } else {
                        value["visibleRequested"] != true
                    };
                    if !identity || !window {
                        return Ok(
                            json!({"overrides":{"nativeReportedCompleted":true,"completed":false,"outcome":"unknown","message":if !identity{"本机终端完成回执未通过身份核验，请检查实际结果，不要自动重做。"}else{"本机终端窗口回执未通过核验，请检查实际结果，不要自动重做。"}},"isError":true,"status":"unknown","code":"HOST_TERMINAL_INVALID_RESULT"}),
                        );
                    }
                    return Ok(json!({"isError":value["exitCode"]!=0}));
                }
                if value["outcome"] == "unknown" {
                    return Ok(
                        json!({"isError":true,"status":"unknown","code":if truthy(&value["cancelled"]){"TOOL_CANCELLED"}else if truthy(&value["timedOut"]){"TOOL_TIMED_OUT"}else if truthy(&value["outputLimitExceeded"]){"HOST_TERMINAL_OUTPUT_LIMIT"}else{"HOST_TERMINAL_OUTCOME_UNKNOWN"}}),
                    );
                }
                return Err(ServiceError::new(
                    if matches(
                        r"^HOST_TERMINAL_[A-Z0-9_]+$",
                        string(&value["error"]["code"]),
                    ) {
                        string(&value["error"]["code"])
                    } else {
                        "HOST_TERMINAL_INVALID_RESULT"
                    },
                    value["error"]["message"]
                        .as_str()
                        .unwrap_or("本机终端未返回执行回执。"),
                    502,
                ));
            }
            "runner.desktopCapabilities" => {
                let value = &arguments["value"];
                if !desktop_envelope(value)
                    || value["available"] != true
                    || value["interactiveWindows"] != true
                    || !value["operations"].is_array()
                    || array(&value["operations"])
                        .iter()
                        .any(|action| !ACTIONS.contains(&string(action)))
                {
                    return Err(ServiceError::new(
                        "DESKTOP_UNAVAILABLE",
                        "当前原生助手没有可用的本机桌面控制能力。",
                        503,
                    ));
                }
                Ok(Value::Null)
            }
            "runner.desktopRequest" => desktop_request(arguments),
            "runner.desktopFile" => {
                if arguments["file"] != true {
                    return Err(failure(
                        "DESKTOP_INVALID_APPLICATION",
                        "应用路径不是本地程序文件。",
                    ));
                }
                Ok(Value::Null)
            }
            "runner.desktopResult" => desktop_result(arguments),
            "runner.desktopInvokeFailure" => {
                if arguments["action"] != "launch" || arguments["unknown"] != true {
                    return Ok(Value::Null);
                }
                Ok(
                    json!({"value":{"action":"launch","boundary":"host-desktop","completed":false,"outcome":"unknown","message":"启动请求没有可核验的完成回执；软件可能已打开，请先检查，不要自动重复启动。"},"isError":true,"status":"unknown","code":if arguments["name"]=="AbortError"{json!("TOOL_CANCELLED")}else{arguments.get("code").cloned().unwrap_or(json!("DESKTOP_OUTCOME_UNKNOWN"))}}),
                )
            }
            "runner.sandboxInit" => {
                self.excluded_roots = array(&arguments["excludedRoots"])
                    .iter()
                    .filter_map(Value::as_str)
                    .map(str::to_owned)
                    .collect();
                self.workspace_home = arguments["conversationWorkspaceHome"]
                    .as_str()
                    .map(str::to_owned);
                Ok(Value::Null)
            }
            "runner.sandboxPaths" => Ok(
                json!({"excludedRoots":self.excluded_roots,"conversationWorkspaceHome":self.workspace_home}),
            ),
            "runner.canonicalRoots" => {
                let canonical = |path: &str| -> ServiceResult<String> {
                    match std::fs::canonicalize(path) {
                        Ok(path) => Ok(path_text(&path)),
                        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                            Ok(path.into())
                        }
                        Err(error) => Err(ServiceError::io(error)),
                    }
                };
                let mut roots = Vec::new();
                for root in array(&arguments["excludedRoots"]) {
                    let root = string(root).to_owned();
                    if !roots.contains(&root) {
                        roots.push(root);
                    }
                }
                let existing = roots.clone();
                for root in existing {
                    let canonical = canonical(&root)?;
                    if !roots.contains(&canonical) {
                        roots.push(canonical);
                    }
                }
                Ok(
                    json!({"excludedRoots":roots,"conversationWorkspaceHome":arguments["conversationWorkspaceHome"].as_str().map(canonical).transpose()?}),
                )
            }
            "runner.sandboxReset" => {
                self.verified_sandbox = false;
                self.verified_skills = false;
                Ok(Value::Null)
            }
            "runner.sandboxVerified" => Ok(json!(self.verified_sandbox)),
            "runner.sandboxCapabilities" => {
                let status = &arguments["value"];
                self.verified_sandbox = status["protocolVersion"] == 1
                    && status["available"] == true
                    && status["sandbox"] == "appcontainer"
                    && status["failClosed"] == true
                    && status["checksChildToken"] == true
                    && status["network"] == false
                    && status["workspaceCopy"] == true;
                if !self.verified_sandbox {
                    return Err(failure(
                        "SANDBOX_UNAVAILABLE",
                        "The native helper does not support the required fail-closed AppContainer protocol.",
                    ));
                }
                self.verified_skills =
                    super::service_skill_environment::SkillEnvironment::default()
                        .apply("environment.supports", &json!({"capabilities":status}))?
                        == true;
                Ok(Value::Null)
            }
            "runner.sandboxRequest" => sandbox_request(arguments),
            "runner.sandboxSkill" => {
                let prepared = &arguments["prepared"];
                if !prepared.is_object()
                    || !prepared["files"].is_array()
                    || array(&prepared["files"]).len() > 128
                    || !prepared["skillRoot"].is_string()
                    || !prepared["scriptRelativePath"].is_string()
                {
                    return Err(failure(
                        "SANDBOX_INVALID_SKILL",
                        "A verified skill package manifest is required.",
                    ));
                }
                Ok(Value::Null)
            }
            "runner.sandboxSkillCapability" => {
                if !self.verified_skills {
                    return Err(failure(
                        "SANDBOX_SKILL_UNSUPPORTED",
                        "The native helper does not advertise hash-checked read-only skill packages.",
                    ));
                }
                Ok(Value::Null)
            }
            "runner.sandboxReceipt" => {
                let result = &arguments["result"];
                if result["protocolVersion"] != 1
                    || result["sandbox"] != "appcontainer"
                    || result["tokenVerified"] != true
                    || result["workspaceCopy"] != true
                    || result["activeProcessesAfterExit"] != 0
                {
                    return Err(failure(
                        "SANDBOX_START_FAILED",
                        "The native helper did not verify isolated execution and process cleanup.",
                    ));
                }
                if !truthy(&result["cancelled"]) {
                    if let Some(stage) = result["stagingDirectory"].as_str() {
                        self.stages.insert(stage.into());
                    }
                }
                Ok(Value::Null)
            }
            "runner.sandboxCancelled" => {
                let result = &arguments["result"];
                let cleanup = arguments["name"] == "AbortError"
                    && result["protocolVersion"] == 1
                    && result["tokenVerified"] == true
                    && result["activeProcessesAfterExit"] == 0
                    && result["cancelled"] == false
                    && result["stagingDirectory"].is_string();
                if cleanup {
                    self.stages
                        .insert(string(&result["stagingDirectory"]).into());
                }
                Ok(json!(cleanup))
            }
            "runner.sandboxStages" => Ok(json!(self.stages)),
            "runner.sandboxOwned" => Ok(json!(self.stages.contains(string(&arguments["stage"])))),
            "runner.sandboxCleanupPlan" => {
                let stage = string(&arguments["stage"]);
                if !self.stages.contains(stage) {
                    return Err(failure(
                        "SANDBOX_INVALID_CLEANUP",
                        "The staging directory does not belong to this runner.",
                    ));
                }
                Ok(json!({"stage":stage}))
            }
            "runner.sandboxCleanup" => cleanup(arguments),
            "runner.sandboxCleaned" => {
                self.stages.remove(string(&arguments["stage"]));
                Ok(Value::Null)
            }
            "runner.cleanupFailure" => {
                let count = if arguments["statuses"].is_array() {
                    array(&arguments["statuses"])
                        .iter()
                        .filter(|status| **status == "rejected")
                        .count() as u64
                } else {
                    arguments["count"].as_u64().unwrap_or(0)
                };
                if count > 0 {
                    return Err(failure(
                        "SANDBOX_CLEANUP_FAILED",
                        &format!("{count} retained sandbox workspace(s) could not be removed."),
                    ));
                }
                Ok(Value::Null)
            }
            _ if operation.starts_with("runner.protocol.") => {
                self.protocol.apply(operation, arguments)
            }
            _ => Err(ServiceError::new(
                "AGENT_CORE_UNKNOWN_OPERATION",
                "Unknown native runner operation.",
                400,
            )),
        }
    }
}

fn sandbox_request(arguments: &Value) -> ServiceResult<Value> {
    let input = &arguments["input"];
    let command = string(&input["command"]);
    let args = array(&input["args"]);
    if !["node", "node.exe", "cmd", "cmd.exe"].contains(&command) {
        return Err(failure(
            "SANDBOX_COMMAND_UNSUPPORTED",
            "Only node and cmd are currently supported in the isolated terminal.",
        ));
    }
    if !input["workspaceRoot"].is_string()
        || arguments["workspaceAbsolute"] != true
        || string(&input["workspaceRoot"]).starts_with(r"\\")
    {
        return Err(failure(
            "SANDBOX_INVALID_WORKSPACE",
            "An absolute local workspace directory is required.",
        ));
    }
    if !input["args"].is_array()
        || args.len() > 128
        || args
            .iter()
            .any(|argument| !argument.is_string() || string(argument).contains('\0'))
        || args
            .iter()
            .map(|argument| string(argument).encode_utf16().count())
            .sum::<usize>()
            > 24000
    {
        return Err(failure(
            "SANDBOX_INVALID_REQUEST",
            "Terminal arguments must be a bounded array of strings.",
        ));
    }
    if args.iter().any(|argument| {
        string(argument).starts_with("--inspect") || string(argument).starts_with("--debug")
    }) {
        return Err(failure(
            "SANDBOX_COMMAND_UNSUPPORTED",
            "Debugger ports are not enabled in the sandbox.",
        ));
    }
    if ["cmd", "cmd.exe"].contains(&command)
        && (args.len() != 3
            || string(&args[0]).to_lowercase() != "/d"
            || string(&args[1]).to_lowercase() != "/c"
            || js_trim(string(&args[2])).is_empty())
    {
        return Err(failure(
            "SANDBOX_INVALID_REQUEST",
            "cmd requires ['/d', '/c', 'command text']; interactive input and AutoRun are disabled.",
        ));
    }
    if !integer(&input["timeoutMs"], 100, 120000) {
        return Err(failure(
            "SANDBOX_INVALID_REQUEST",
            "Timeout must be between 100 and 120000 milliseconds.",
        ));
    }
    Ok(Value::Null)
}

fn cleanup(arguments: &Value) -> ServiceResult<Value> {
    let invalid = || {
        failure(
            "SANDBOX_INVALID_CLEANUP",
            "Refusing cleanup outside an owned sandbox run.",
        )
    };
    let root = std::env::temp_dir().join("kynxa-tool-sandbox");
    inspect(&root, false, false)?;
    let root = std::fs::canonicalize(root).map_err(ServiceError::io)?;
    let stage = absolute_path(string(&arguments["stage"]));
    let requested = stage.parent().ok_or_else(invalid)?;
    inspect(requested, false, false)?;
    let run = std::fs::canonicalize(requested).map_err(ServiceError::io)?;
    if !within(&root, &run) || run == root || run.parent() != Some(root.as_path()) {
        return Err(invalid());
    }
    let metadata = inspect(&run, false, false)?.ok_or_else(invalid)?;
    if !metadata.is_dir() {
        return Err(failure(
            "SANDBOX_INVALID_CLEANUP",
            "The run directory was replaced by a link.",
        ));
    }
    std::fs::remove_dir_all(run).map_err(ServiceError::io)?;
    Ok(Value::Null)
}

fn desktop_request(arguments: &Value) -> ServiceResult<Value> {
    let action = string(&arguments["action"]);
    let input = &arguments["input"];
    if !ACTIONS.contains(&action) {
        return Err(failure("DESKTOP_ACTION_UNSUPPORTED", "不支持此本机操作。"));
    }
    for name in ["crop", "region"] {
        if let Some(area) = input.get(name) {
            if !area.is_object()
                || area
                    .as_object()
                    .unwrap()
                    .keys()
                    .any(|key| !["x", "y", "width", "height"].contains(&key.as_str()))
                || !integer(&area["x"], 0, 32767)
                || !integer(&area["y"], 0, 32767)
                || !integer(&area["width"], 1, 8192)
                || !integer(&area["height"], 1, 8192)
            {
                return Err(failure(
                    "DESKTOP_INVALID_COORDINATES",
                    "截图或读取区域需要有效的客户区像素坐标和尺寸。",
                ));
            }
        }
    }
    if action == "read"
        && (input
            .get("timeoutMs")
            .is_some_and(|value| !integer(value, 500, 5000))
            || input.get("elementId").is_some_and(|value| {
                !value.is_string()
                    || string(value).encode_utf16().count() > 256
                    || !matches(r"^-?\d+(?:,-?\d+){0,31}$", string(value))
            }))
    {
        return Err(failure(
            "DESKTOP_INVALID_REQUEST",
            "读取期限或元素标识无效。",
        ));
    }
    if action == "window"
        && (!["resize", "maximize", "minimize", "restore"].contains(&string(&input["mode"]))
            || input["mode"] == "resize"
                && (!integer(&input["width"], 64, 8192) || !integer(&input["height"], 64, 8192)))
    {
        return Err(failure(
            "DESKTOP_INVALID_REQUEST",
            "窗口调整需要有效模式；调整大小时需指定客户区宽高。",
        ));
    }
    if action == "key" && !array(&arguments["keys"]).contains(&input["key"]) {
        return Err(failure("DESKTOP_INVALID_KEY", "不支持此按键组合。"));
    }
    if action == "launch"
        && input
            .get("background")
            .is_some_and(|value| !value.is_boolean())
    {
        return Err(failure(
            "DESKTOP_INVALID_REQUEST",
            "后台启动选项必须为布尔值。",
        ));
    }
    if action == "launch" {
        let path = string(&input["appPath"]);
        if !input["appPath"].is_string()
            || arguments["pathAbsolute"] != true
            || path.starts_with(r"\\")
            || !matches(r"(?i)\.exe$", path)
            || matches(
                r"(?i)^(?:cmd|powershell|pwsh|wscript|cscript|mshta|rundll32|regsvr32|node|python(?:\d+(?:\.\d+)*)?|py|bash|sh|wsl|wt)\.exe$",
                string(&arguments["basename"]),
            )
        {
            return Err(failure(
                "DESKTOP_INVALID_APPLICATION",
                "打开软件需要本地应用 .exe 路径；命令解释器请使用对应的终端工具。",
            ));
        }
    } else if !["windows", "apps"].contains(&action)
        && (!matches(r"^[1-9]\d{0,19}$", string(&input["windowId"]))
            || !integer(&input["processId"], 1, 9_007_199_254_740_991))
    {
        return Err(failure(
            "DESKTOP_INVALID_TARGET",
            "请使用 computer.windows 返回的窗口与进程身份。",
        ));
    }
    Ok(
        json!({"timeoutMs":if action=="read"{input["timeoutMs"].as_u64().unwrap_or(3000)+1500}else{20000}}),
    )
}

fn desktop_result(arguments: &Value) -> ServiceResult<Value> {
    let action = string(&arguments["action"]);
    let value = &arguments["value"];
    let parameters = &arguments["parameters"];
    if value["error"]["partial"] == true
        && value["error"]["deliveredInputEvents"]
            .as_i64()
            .is_some_and(|value| value > 0)
    {
        return Ok(
            json!({"kind":"partial","projectedValue":{"action":action,"boundary":"host-desktop","completed":false,"partial":true,"deliveredInputEvents":value["error"]["deliveredInputEvents"],"message":value["error"]["message"]},"isError":true,"status":"unknown","code":if arguments["aborted"]==true{"TOOL_CANCELLED"}else{"DESKTOP_PARTIAL_INPUT"}}),
        );
    }
    let region_matches = |expected: &Value, actual: &Value| {
        actual.is_object()
            && ["x", "y", "width", "height"]
                .iter()
                .all(|key| expected[*key] == actual[*key])
    };
    let mismatch = action == "launch"
        && parameters.get("background").is_some()
        && (value["backgroundRequested"] != parameters["background"]
            || value["backgroundMode"]
                != if parameters["background"] == true {
                    "best-effort-no-activate"
                } else {
                    "normal"
                })
        || action == "window" && value["mode"] != parameters["mode"]
        || action == "screenshot"
            && truthy(&parameters["crop"])
            && (!region_matches(&parameters["crop"], &value["crop"])
                || value["width"] != parameters["crop"]["width"]
                || value["height"] != parameters["crop"]["height"])
        || action == "read"
            && (truthy(&parameters["region"])
                && !region_matches(&parameters["region"], &value["region"])
                || parameters.get("elementId").is_some()
                    && value["elementId"] != parameters["elementId"]
                || parameters.get("timeoutMs").is_some()
                    && value["timeoutMs"] != parameters["timeoutMs"]);
    if !desktop_envelope(value)
        || value["action"] != action
        || value["completed"] != true
        || mismatch
    {
        if action == "launch" {
            return Ok(
                json!({"kind":"unconfirmed","projectedValue":{"action":action,"boundary":"host-desktop","completed":false,"outcome":"unknown","nativeReportedCompleted":value["completed"]==true,"message":"启动回执未通过身份核验；软件可能已打开，请先检查，不要自动重复启动。"},"isError":true,"status":"unknown","code":"DESKTOP_INVALID_RESULT"}),
            );
        }
        return Err(ServiceError::new(
            "DESKTOP_INVALID_RESULT",
            "原生本机操作缺少完成回执。",
            502,
        ));
    }
    if action == "screenshot" {
        let encoded = string(&value["data"]);
        if value["mimeType"] != "image/png"
            || !value["data"].is_string()
            || !matches(r"^[A-Za-z0-9+/]+={0,2}$", encoded)
            || !integer(&value["width"], 1, 9_007_199_254_740_991)
            || !integer(&value["height"], 1, 9_007_199_254_740_991)
            || value["width"].as_u64().unwrap() as u128 * value["height"].as_u64().unwrap() as u128
                > 16_000_000
        {
            return Err(ServiceError::new(
                "DESKTOP_INVALID_RESULT",
                "本机截图格式或尺寸不受支持。",
                502,
            ));
        }
        let bytes = STANDARD.decode(encoded).ok().filter(|bytes| {
            bytes.len() <= 4 * 1024 * 1024
                && STANDARD.encode(bytes) == encoded
                && bytes.starts_with(&[137, 80, 78, 71, 13, 10, 26, 10])
        });
        if bytes.is_none() {
            return Err(ServiceError::new(
                "DESKTOP_INVALID_RESULT",
                "本机截图不是有效的 PNG 编码。",
                502,
            ));
        }
        return Ok(
            json!({"kind":"screenshot","imagePreview":"PNG saved; inspect in the local tool result viewer. No image pixels were sent to the text model.","isError":false}),
        );
    }
    Ok(json!({"kind":"value","isError":false}))
}

#[derive(Default)]
struct ProtocolState {
    kind: String,
    request: Value,
    stdout_bytes: u64,
    stderr_bytes: u64,
    value: Option<Value>,
    started: Option<Value>,
    sequence: u64,
    partial: HashMap<String, Vec<u16>>,
    frames: usize,
    invalid: Option<Value>,
}

impl ProtocolState {
    fn apply(&mut self, operation: &str, arguments: &Value) -> ServiceResult<Value> {
        match operation {
            "runner.protocol.callbackFailure" => {
                if self.kind == "sandbox" {
                    return Err(failure(
                        "SANDBOX_START_FAILED",
                        string(&arguments["message"]),
                    ));
                }
                Err(ServiceError::new(
                    if arguments["channel"] == "started" {
                        "HOST_TERMINAL_INVALID_RESULT"
                    } else {
                        "HOST_TERMINAL_DISPLAY_CLOSED"
                    },
                    if arguments["channel"] == "started" {
                        "本机终端任务启动回执无法保存。"
                    } else {
                        "本机终端显示通道已关闭。"
                    },
                    502,
                ))
            }
            "runner.protocol.timeoutFailure" => Ok(if self.kind == "desktop" {
                ServiceError::new("DESKTOP_TIMED_OUT", "本机操作超时。", 504).value()
            } else if self.kind == "host" {
                ServiceError::new("HOST_TERMINAL_TIMED_OUT", "本机终端超时。", 504).value()
            } else {
                failure(
                    "SANDBOX_HOST_TIMEOUT",
                    "The native sandbox helper exceeded its startup or cleanup deadline.",
                )
                .value()
            }),
            "runner.protocol.cancelFailure" => Ok(
                json!({"name":"AbortError","message":if self.kind=="desktop"{"本机操作已取消。"}else if self.kind=="host"{"本机终端已取消。"}else{"Sandbox execution was cancelled."}}),
            ),
            "runner.protocol.channelFailure" => Err(ServiceError::new(
                "DESKTOP_INVALID_RESULT",
                if arguments["channel"] == "stdout" {
                    "本机助手结果连接中断。"
                } else {
                    "本机助手错误连接中断。"
                },
                502,
            )),
            "runner.protocol.init" => {
                self.kind = string(&arguments["kind"]).into();
                self.request = arguments["request"].clone();
                Ok(
                    json!({"hardStopMs":if self.kind=="desktop"{1000}else if self.kind=="host"{4000}else{5000},"drainMs":100,"maximumFrameBytes":2*1024*1024,"dispatches":(["host_terminal","host_terminal_visible","host_terminal_job"].contains(&string(&self.request["operation"]))),"endInput":self.request["operation"]=="capabilities"}),
                )
            }
            "runner.protocol.bytes" => {
                let bytes = arguments["bytes"].as_u64().unwrap_or(0);
                if arguments["stream"] == "stderr" {
                    self.stderr_bytes = self.stderr_bytes.saturating_add(bytes);
                } else {
                    self.stdout_bytes = self.stdout_bytes.saturating_add(bytes);
                }
                if self.kind == "desktop" && self.stdout_bytes > 8 * 1024 * 1024 {
                    let failure = ServiceError::new(
                        "DESKTOP_RESULT_TOO_LARGE",
                        "本机结果超过保存上限。",
                        413,
                    );
                    self.invalid = Some(failure.value());
                    return Err(failure);
                }
                if self.kind == "desktop" && self.stderr_bytes > 65536 {
                    let failure =
                        ServiceError::new("DESKTOP_INVALID_RESULT", "本机助手错误输出过大。", 502);
                    self.invalid = Some(failure.value());
                    return Err(failure);
                }
                if self.kind == "host" && self.stderr_bytes > 65536 {
                    return Err(ServiceError::new(
                        "HOST_TERMINAL_INVALID_RESULT",
                        "本机终端协议错误。",
                        502,
                    ));
                }
                if self.kind == "sandbox" && self.stdout_bytes > 2 * 1024 * 1024 {
                    return Err(failure(
                        "SANDBOX_HOST_OUTPUT_LIMIT",
                        "The native helper returned an oversized response.",
                    ));
                }
                Ok(Value::Null)
            }
            "runner.protocol.frameLimit" => {
                if arguments["bytes"].as_u64().unwrap_or(0) > 2 * 1024 * 1024 {
                    return Err(ServiceError::new(
                        "HOST_TERMINAL_RESULT_TOO_LARGE",
                        "本机终端回执过大。",
                        413,
                    ));
                }
                Ok(Value::Null)
            }
            "runner.protocol.frame" => {
                let value = &arguments["value"];
                if arguments["empty"] == true {
                    return Ok(json!({"action":"ignore"}));
                }
                if self.kind == "desktop" {
                    self.frames += 1;
                    if self.frames != 1 || arguments["parsed"] != true {
                        let failure = ServiceError::new(
                            "DESKTOP_INVALID_RESULT",
                            "本机助手未返回有效结果。",
                            502,
                        );
                        self.invalid = Some(failure.value());
                        return Err(failure);
                    }
                    self.value = Some(value.clone());
                    return Ok(json!({"action":"reply"}));
                }
                if self.kind == "sandbox" {
                    if arguments["parsed"] != true {
                        return Err(failure(
                            "SANDBOX_START_FAILED",
                            arguments["message"]
                                .as_str()
                                .unwrap_or("Invalid JSON response."),
                        ));
                    }
                    if value["event"] == "sandbox_started"
                        && value["protocolVersion"] == 1
                        && self.request["operation"] == "run"
                    {
                        return Ok(json!({"action":"started"}));
                    }
                    if truthy(&value["event"]) || self.value.is_some() {
                        return Err(failure(
                            "SANDBOX_START_FAILED",
                            "The native helper returned an invalid or repeated record.",
                        ));
                    }
                    self.value = Some(value.clone());
                    return Ok(json!({"action":"reply"}));
                }
                if arguments["parsed"] != true || !host_envelope(value) {
                    return Ok(json!({"action":"ignore"}));
                }
                if value["event"] == "host_terminal_started"
                    && value["processId"].as_i64().is_some_and(|pid| pid > 0)
                {
                    if self.started.is_some() {
                        return Err(ServiceError::new(
                            "HOST_TERMINAL_INVALID_RESULT",
                            "本机终端重复启动回执。",
                            502,
                        ));
                    }
                    self.started = Some(value.clone());
                    return Ok(json!({"action":"started"}));
                }
                if value["event"] == "host_terminal_output" {
                    let stream = string(&value["stream"]);
                    let replace = stream == "console" && value["replace"] == true;
                    let text = if replace {
                        &value["text"]
                    } else {
                        &value["delta"]
                    };
                    if self.started.is_none()
                        || !integer(
                            &value["sequence"],
                            self.sequence.saturating_add(1),
                            9_007_199_254_740_991,
                        )
                        || !["stdout", "stderr", "console"].contains(&stream)
                        || stream == "console" && !replace
                        || !text.is_string()
                        || string(text).encode_utf16().count() > 65536
                    {
                        return Err(ServiceError::new(
                            "HOST_TERMINAL_INVALID_RESULT",
                            "本机终端输出事件无效。",
                            502,
                        ));
                    }
                    self.sequence = value["sequence"].as_u64().unwrap();
                    let key = if stream == "console" {
                        "consoleText"
                    } else {
                        stream
                    };
                    let units = self.partial.entry(key.into()).or_default();
                    if replace {
                        units.clear();
                    }
                    units.extend(string(text).encode_utf16());
                    if !replace && units.len() > 262144 {
                        units.drain(..units.len() - 262144);
                    }
                    return Ok(
                        json!({"action":"output","output":{"sequence":self.sequence,"stream":stream,"text":text,"replace":replace}}),
                    );
                }
                self.value = Some(value.clone());
                Ok(json!({"action":"reply"}))
            }
            "runner.protocol.hasReply" => Ok(json!(self.value.is_some())),
            "runner.protocol.invalid" => Ok(json!(self.invalid.is_some())),
            "runner.protocol.finish" => self.finish(arguments),
            _ => Err(ServiceError::new(
                "AGENT_CORE_UNKNOWN_OPERATION",
                "Unknown ToolHost protocol operation.",
                400,
            )),
        }
    }
    fn finish(&self, arguments: &Value) -> ServiceResult<Value> {
        let empty = Value::Null;
        let value = self.value.as_ref().unwrap_or(&empty);
        let stop = &arguments["stopError"];
        if self.kind == "sandbox" {
            if arguments["pendingNonempty"] == true || self.value.is_none() {
                return Ok(
                    json!({"action":"failure","abort":arguments["aborted"]==true,"code":"SANDBOX_START_FAILED","message":format!("The native helper exited without a valid response ({}).",arguments["exitCode"])}),
                );
            }
            if arguments["aborted"] == true || truthy(&value["cancelled"]) {
                return Ok(json!({"action":"abort","value":value}));
            }
            if truthy(&value["error"]) {
                return Ok(
                    json!({"action":"failure","code":value["error"]["code"].as_str().unwrap_or("SANDBOX_START_FAILED"),"message":value["error"]["message"]}),
                );
            }
            if arguments["exitCode"] != 0 {
                return Ok(
                    json!({"action":"failure","code":"SANDBOX_START_FAILED","message":format!("The native helper failed ({}).",arguments["exitCode"])}),
                );
            }
            return Ok(json!({"action":"value","value":value}));
        }
        if self.kind == "host" {
            if self.request["operation"] == "host_terminal_capabilities"
                && stop.is_null()
                && host_envelope(value)
                && value["available"].is_boolean()
                || host_envelope(value)
                    && (value["completed"] == true
                        || value["outcome"] == "unknown"
                        || value["outcome"] == "not_started")
            {
                return Ok(json!({"action":"value","value":value}));
            }
            if self.started.is_some() || arguments["dispatched"] == true {
                let mut unknown = value
                    .as_object()
                    .cloned()
                    .map(Value::Object)
                    .unwrap_or(json!({}));
                unknown["protocolVersion"] = json!(1);
                unknown["boundary"] = json!("host-terminal");
                for name in ["stdout", "stderr", "consoleText"] {
                    unknown[name] = json!(String::from_utf16_lossy(
                        self.partial.get(name).map(Vec::as_slice).unwrap_or(&[])
                    ));
                }
                unknown["completed"] = json!(false);
                unknown["outcome"] = json!("unknown");
                if let Some(started) = &self.started {
                    unknown["processId"] = started["processId"].clone();
                }
                unknown["cancelled"] = json!(stop["name"] == "AbortError");
                unknown["timedOut"] = json!(stop["code"] == "HOST_TERMINAL_TIMED_OUT");
                unknown["message"] = json!(
                    "命令已发送，但没有完整完成回执，执行结果未知；请核验结果，不要自动重做。"
                );
                return Ok(json!({"action":"value","value":unknown}));
            }
            if !stop.is_null() {
                return Ok(json!({"action":"stop"}));
            }
            if host_envelope(value) {
                return Ok(json!({"action":"value","value":value}));
            }
            return Err(ServiceError::new(
                "HOST_TERMINAL_INVALID_RESULT",
                "本机终端助手未返回有效结果。",
                502,
            ));
        }
        if stop.is_null()
            && self.request["operation"] == "desktop_capabilities"
            && desktop_envelope(value)
            && value["available"].is_boolean()
            && !truthy(&value["error"])
        {
            return Ok(json!({"action":"value","value":value}));
        }
        if self.invalid.is_none()
            && (stop.is_null()
                || stop["name"] == "AbortError"
                || stop["code"] == "DESKTOP_TIMED_OUT")
            && desktop_envelope(value)
            && value["completed"] == true
            && !truthy(&value["error"])
        {
            return Ok(json!({"action":"value","value":value}));
        }
        if value["error"]["partial"] == true
            && value["error"]["deliveredInputEvents"]
                .as_i64()
                .is_some_and(|count| count > 0)
        {
            return Ok(json!({"action":"value","value":value}));
        }
        let uncertain = arguments["spawned"] == true
            && arguments["submitted"] == true
            && self.request["operation"] == "desktop";
        if let Some(invalid) = &self.invalid {
            return Ok(json!({"action":"failure","failure":invalid,"uncertain":uncertain}));
        }
        if !stop.is_null() {
            return Ok(json!({"action":"stop","uncertain":uncertain}));
        }
        let error = &value["error"];
        let failure = ServiceError::new(
            if matches(r"^DESKTOP_[A-Z0-9_]+$", string(&error["code"])) {
                string(&error["code"])
            } else {
                "DESKTOP_INVALID_RESULT"
            },
            error["message"]
                .as_str()
                .unwrap_or("本机助手未返回有效结果。"),
            502,
        );
        let unconfirmed = error.is_null()
            || error["code"] == "DESKTOP_TIMEOUT"
            || self.request["action"] == "launch"
                && ![
                    "DESKTOP_LAUNCH_BLOCKED",
                    "DESKTOP_LAUNCH_FAILED",
                    "DESKTOP_INVALID_REQUEST",
                    "DESKTOP_UNAVAILABLE",
                    "DESKTOP_BUSY",
                    "DESKTOP_CANCELLED",
                    "DESKTOP_ACCESS_DENIED",
                ]
                .contains(&string(&error["code"]));
        Ok(json!({"action":"failure","failure":failure.value(),"uncertain":unconfirmed&&uncertain}))
    }
}

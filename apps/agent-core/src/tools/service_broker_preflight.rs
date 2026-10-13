use super::service_error::{ServiceError, ServiceResult};
use crate::models::protocol::{string, truthy};
use serde_json::{Value, json};

const MAX_INPUT_BYTES: u64 = 2 * 1024 * 1024;
const FILESYSTEM_READS: &[&str] = &[
    "filesystem.read",
    "filesystem.list",
    "filesystem.search",
    "filesystem.stat",
];

fn failure(message: &str, code: &str, status: u16) -> ServiceError {
    ServiceError::new(code, message, status)
}
fn has_text(value: &Value) -> bool {
    value.as_str().is_some_and(|value| {
        !value
            .trim_matches(|character: char| {
                character == '\u{feff}' || character.is_whitespace() && character != '\u{0085}'
            })
            .is_empty()
    })
}
fn required_reason(kind: &str, value: &Value) -> ServiceResult<()> {
    if has_text(value) && (kind != "mcp" || string(value).encode_utf16().count() <= 2000) {
        return Ok(());
    }
    Err(failure(
        match kind {
            "web" => "读取外部网页必须说明原因。",
            "mcp" => "调用外部 MCP 服务必须说明原因。",
            "host" => "本机终端操作必须说明原因。",
            "desktop" => "本机桌面操作必须说明原因。",
            _ => "访问工作范围以外的真实路径必须说明原因。",
        },
        "OUTSIDE_WORKSPACE_REASON_REQUIRED",
        403,
    ))
}
fn verified_sandbox(capabilities: &Value) -> bool {
    capabilities["available"] == true
        && capabilities["sandbox"] == "appcontainer"
        && capabilities["failClosed"] == true
        && capabilities["checksChildToken"] == true
}
fn filesystem(arguments: &Value) -> ServiceResult<Value> {
    let name = string(&arguments["name"]);
    let reading = FILESYSTEM_READS.contains(&name);
    let protected_write = arguments["readOnlyExtension"] == true
        || arguments["owned"] == true && arguments["managedPath"] != true
        || arguments["bundled"] == true;
    if arguments["phase"] == "requested" {
        if !reading && protected_write {
            return Err(failure(
                "正式应用数据和内置技能由专用服务管理，文件工具不能改写。",
                "PROTECTED_APP_DATA",
                403,
            ));
        }
        return Ok(
            json!({"reading":reading,"allowMissing":(["filesystem.write","filesystem.mkdir"].contains(&name))}),
        );
    }
    let mut outside = arguments["outsideWorkspace"] == true;
    if outside {
        required_reason("filesystem", &arguments["reason"])?;
    }
    if arguments["control"] == true {
        return Err(failure(
            "聊天工具目录的归属信息由应用管理。",
            "PROTECTED_APP_DATA",
            403,
        ));
    }
    if !reading && protected_write {
        return Err(failure(
            "正式应用数据和内置技能由专用服务管理，文件工具不能改写。",
            "PROTECTED_APP_DATA",
            403,
        ));
    }
    if reading && arguments["credential"] == true {
        return Err(failure(
            "模型或工具连接和备份可能含密钥，文件工具不能读取。",
            "PROTECTED_MODEL_CREDENTIALS",
            403,
        ));
    }
    if reading && arguments["privateResult"] == true {
        return Err(failure(
            "完整工具结果须使用专用公开投影读取。",
            "PROTECTED_TOOL_RESULT",
            403,
        ));
    }
    if reading && arguments["owned"] == true && arguments["managedPath"] != true {
        outside = true;
        if arguments["reasonIsString"] == false || !has_text(&arguments["reason"]) {
            return Err(failure(
                "读取正式应用数据必须说明原因。",
                "OUTSIDE_WORKSPACE_REASON_REQUIRED",
                403,
            ));
        }
    }
    Ok(
        json!({"outsideWorkspace":outside,"sensitiveRead":reading && name!="filesystem.list" && arguments["sensitive"]==true}),
    )
}
fn host(arguments: &Value) -> ServiceResult<Value> {
    let input = &arguments["input"];
    let capabilities = &arguments["capabilities"];
    required_reason("host", &input["reason"])?;
    if arguments["runnerAvailable"] != true
        || capabilities["available"] != true
        || capabilities["boundary"] != "host-terminal"
        || !capabilities["shells"]
            .as_array()
            .is_some_and(|shells| shells.contains(&input["shell"]))
    {
        return Err(failure(
            "当前本机终端不可用。",
            "HOST_TERMINAL_UNAVAILABLE",
            503,
        ));
    }
    if arguments["name"] == "terminal.host.start" && capabilities["backgroundJobs"] != true {
        return Err(failure(
            "当前本机助手尚不支持后台终端任务。",
            "HOST_TERMINAL_BACKGROUND_UNAVAILABLE",
            503,
        ));
    }
    if input["visible"] == true
        && (capabilities["protocolVersion"] != 2 || capabilities["visibleTerminal"] != true)
    {
        return Err(failure(
            "当前原生助手不支持可见终端，请更新后重试。",
            "HOST_TERMINAL_VISIBLE_UNAVAILABLE",
            503,
        ));
    }
    if input["visible"] != true && arguments["hasKeepOpen"] == true {
        return Err(failure(
            "窗口保留时间仅适用于可见终端。",
            "HOST_TERMINAL_INVALID_REQUEST",
            400,
        ));
    }
    if input["visible"] == true && arguments["foregroundForbidden"] == true {
        return Err(failure(
            "用户要求保持后台，不能打开可见终端窗口。",
            "DESKTOP_FOREGROUND_FORBIDDEN",
            403,
        ));
    }
    if arguments["absolute"] != true || arguments["unc"] == true {
        return Err(failure(
            "本机终端需要有效的绝对本地工作目录。",
            "HOST_TERMINAL_INVALID_WORKSPACE",
            400,
        ));
    }
    Ok(json!({"outsideWorkspace":true}))
}
fn sandbox(arguments: &Value) -> ServiceResult<Value> {
    let capabilities = &arguments["capabilities"];
    if arguments["ownedWorkspace"] == true && arguments["managedWorkspace"] != true
        || arguments["managedExtension"] == true
    {
        return Err(failure(
            "正式应用数据不能作为终端工作范围。",
            "PROTECTED_APP_DATA",
            403,
        ));
    }
    if arguments["runnerAvailable"] != true || !verified_sandbox(capabilities) {
        return Err(failure(
            "已验证的 AppContainer 沙箱不可用，未在宿主执行。",
            "SANDBOX_UNAVAILABLE",
            503,
        ));
    }
    let skill = arguments["name"] == "skill.run";
    let command = if skill {
        "node"
    } else {
        string(&arguments["input"]["command"])
            .strip_suffix(".exe")
            .unwrap_or(string(&arguments["input"]["command"]))
    };
    if !capabilities["commands"]
        .as_array()
        .is_some_and(|commands| commands.iter().any(|value| value == command))
    {
        return Err(failure(
            "此命令未由已验证沙箱声明支持。",
            "SANDBOX_COMMAND_UNSUPPORTED",
            400,
        ));
    }
    if command == "cmd" {
        let arguments = arguments["input"]["args"].as_array();
        if !arguments.is_some_and(|arguments| {
            arguments.len() == 3
                && string(&arguments[0]).eq_ignore_ascii_case("/d")
                && string(&arguments[1]).eq_ignore_ascii_case("/c")
                && has_text(&arguments[2])
        }) {
            return Err(failure(
                "cmd 必须使用 /d /c 和一条命令文本。",
                "INVALID_TOOL_ARGUMENTS",
                400,
            ));
        }
    }
    if skill {
        if !super::service_skill_environment::supports_execution(capabilities) {
            return Err(failure(
                "当前沙箱助手尚不支持经验证的技能包执行。",
                "SANDBOX_SKILL_UNSUPPORTED",
                503,
            ));
        }
        if arguments["runSkillAvailable"] != true {
            return Err(failure(
                "技能沙箱执行器不可用。",
                "SANDBOX_UNAVAILABLE",
                503,
            ));
        }
    }
    Ok(json!({"verifiedSandbox":true,"command":command,"skill":skill}))
}

/// Host and path effects provide observed facts; native admission owns their ordered failure contracts.
/// 宿主及路径效应仅提供已观测事实；原生准入拥有顺序一致的拒绝合同。
pub(super) fn apply(operation: &str, arguments: &Value) -> ServiceResult<Value> {
    match operation {
        "broker.preflight.call" => {
            if let Some(bytes) = arguments.get("inputBytes") {
                if bytes.as_u64().is_none_or(|bytes| bytes > MAX_INPUT_BYTES) {
                    return Err(failure("工具参数过大。", "INVALID_TOOL_ARGUMENTS", 400));
                }
            } else if arguments["idIsString"] == false
                || arguments["nameIsString"] == false
                || !arguments["id"].as_str().is_some_and(|id| {
                    !id.is_empty()
                        && id.encode_utf16().count() <= 128
                        && !id.contains(['\0', '\r', '\n'])
                })
                || !arguments["name"].is_string()
            {
                return Err(failure("工具调用身份无效。", "INVALID_TOOL_ARGUMENTS", 400));
            }
            Ok(Value::Null)
        }
        "broker.preflight.browserRead" => {
            if arguments["automatic"] == true && arguments["browserRead"] == "off" {
                return Err(failure(
                    "自动浏览器阅读已关闭；请使用公开网页读取或用户明确要求的浏览器操作。",
                    "WEB_BROWSER_READ_DISABLED",
                    409,
                ));
            }
            Ok(Value::Null)
        }
        "broker.preflight.filesystem" => filesystem(arguments),
        "broker.preflight.reason" => {
            if arguments["kind"] == "mcp" && arguments["reasonIsString"] == false {
                return Err(failure(
                    "调用外部 MCP 服务必须说明原因。",
                    "OUTSIDE_WORKSPACE_REASON_REQUIRED",
                    403,
                ));
            }
            required_reason(string(&arguments["kind"]), &arguments["reason"])?;
            Ok(Value::Null)
        }
        "broker.preflight.host" => host(arguments),
        "broker.preflight.hostWorkspace" => {
            if arguments["directory"] != true {
                return Err(failure(
                    "本机终端需要有效的绝对本地工作目录。",
                    "HOST_TERMINAL_INVALID_WORKSPACE",
                    400,
                ));
            }
            Ok(Value::Null)
        }
        "broker.preflight.hostJob" => {
            if arguments["available"] != true {
                return Err(failure(
                    "当前本机终端不可用。",
                    "HOST_TERMINAL_UNAVAILABLE",
                    503,
                ));
            }
            Ok(json!({"outsideWorkspace":arguments["name"]=="terminal.host.stop"}))
        }
        "broker.preflight.desktop" => {
            let action = string(&arguments["name"])
                .strip_prefix("computer.")
                .unwrap_or("");
            let force_background = action == "launch" && arguments["foregroundForbidden"] == true;
            if force_background && arguments["input"]["background"] == false {
                return Err(failure(
                    "用户要求保持后台，不能使用前台启动。",
                    "DESKTOP_FOREGROUND_FORBIDDEN",
                    403,
                ));
            }
            let capabilities = &arguments["capabilities"];
            if arguments["runnerAvailable"] != true
                || capabilities["available"] != true
                || capabilities["boundary"] != "host-desktop"
                || !capabilities["operations"]
                    .as_array()
                    .is_some_and(|operations| operations.iter().any(|value| value == action))
            {
                return Err(failure(
                    "当前本机桌面操作不可用。",
                    "DESKTOP_UNAVAILABLE",
                    503,
                ));
            }
            Ok(json!({"outsideWorkspace":true,"forceBackground":force_background}))
        }
        "broker.preflight.sandboxState" => Ok(json!(verified_sandbox(&arguments["capabilities"]))),
        "broker.preflight.sandboxWorkspace" => {
            if truthy(&arguments["workspaceDiagnostic"]) {
                return Err(failure(
                    "关联工作文件夹暂时不可用，请更新挂载路径；其他能力可以继续。",
                    "WORKSPACE_UNAVAILABLE",
                    409,
                ));
            }
            Ok(Value::Null)
        }
        "broker.preflight.sandbox" => sandbox(arguments),
        "broker.preflight.skillEnvironment" => {
            if !truthy(&arguments["canRun"]) {
                return Err(failure(
                    "技能运行环境或依赖尚未满足，请先查看 skill.check 的诊断。",
                    "APP_SKILL_ENVIRONMENT_UNAVAILABLE",
                    409,
                ));
            }
            Ok(Value::Null)
        }
        _ => Err(failure(
            "未知工具准入阶段。",
            "AGENT_CORE_UNKNOWN_OPERATION",
            400,
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn protected_reads_keep_original_failure_order_and_required_reason() {
        assert_eq!(
            apply(
                "broker.preflight.call",
                &json!({"id":"boxed-id","name":"filesystem.read","idIsString":false})
            )
            .unwrap_err()
            .code,
            "INVALID_TOOL_ARGUMENTS"
        );
        let credential = json!({"name":"filesystem.read","phase":"bound","outsideWorkspace":true,
            "credential":true,"owned":true,"reason":""});
        assert_eq!(
            apply("broker.preflight.filesystem", &credential)
                .unwrap_err()
                .code,
            "OUTSIDE_WORKSPACE_REASON_REQUIRED"
        );
        let mut credential = credential;
        credential["reason"] = json!("Inspect the requested file");
        assert_eq!(
            apply("broker.preflight.filesystem", &credential)
                .unwrap_err()
                .code,
            "PROTECTED_MODEL_CREDENTIALS"
        );
        credential["credential"] = json!(false);
        credential["outsideWorkspace"] = json!(false);
        assert_eq!(
            apply("broker.preflight.filesystem", &credential).unwrap()["outsideWorkspace"],
            true
        );
        assert_eq!(
            apply(
                "broker.preflight.reason",
                &json!({"kind":"mcp","reason":"\u{feff} "})
            )
            .unwrap_err()
            .code,
            "OUTSIDE_WORKSPACE_REASON_REQUIRED"
        );
        assert!(
            apply(
                "broker.preflight.reason",
                &json!({"kind":"mcp","reason":"\u{0085}"})
            )
            .is_ok()
        );
        assert_eq!(
            apply(
                "broker.preflight.reason",
                &json!({"kind":"mcp","reason":"boxed reason","reasonIsString":false})
            )
            .unwrap_err()
            .code,
            "OUTSIDE_WORKSPACE_REASON_REQUIRED"
        );
    }

    #[test]
    fn host_and_sandbox_admission_never_accept_unverified_capabilities() {
        let mut host = json!({"name":"terminal.host.start","input":{"shell":"powershell","reason":"requested command","visible":true},
            "capabilities":{"available":true,"boundary":"host-terminal","shells":["powershell"],"backgroundJobs":true,
                "protocolVersion":2,"visibleTerminal":true},"runnerAvailable":true,"absolute":true});
        assert!(apply("broker.preflight.host", &host).is_ok());
        host["foregroundForbidden"] = json!(true);
        assert_eq!(
            apply("broker.preflight.host", &host).unwrap_err().code,
            "DESKTOP_FOREGROUND_FORBIDDEN"
        );
        let mut sandbox = json!({"name":"terminal.run","input":{"command":"cmd.exe","args":["/D","/C","echo fixture"]},
            "capabilities":{"available":true,"sandbox":"appcontainer","failClosed":true,"checksChildToken":true,"commands":["cmd"]},"runnerAvailable":true});
        assert_eq!(
            apply("broker.preflight.sandbox", &sandbox).unwrap()["command"],
            "cmd"
        );
        sandbox["input"]["args"] = json!(["/C", "echo fixture"]);
        assert_eq!(
            apply("broker.preflight.sandbox", &sandbox)
                .unwrap_err()
                .code,
            "INVALID_TOOL_ARGUMENTS"
        );
        sandbox["capabilities"]["checksChildToken"] = json!(false);
        assert_eq!(
            apply("broker.preflight.sandbox", &sandbox)
                .unwrap_err()
                .code,
            "SANDBOX_UNAVAILABLE"
        );
    }
}

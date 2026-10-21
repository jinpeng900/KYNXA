//! Conversation-owned ConPTY sessions share raw input and tracked shell commands with the visible terminal.
//! 聊天自有 ConPTY 会话与可见终端共享原始输入及有回执的命令，工作目录由正式归属解析。

use super::gateway::{GatewayToolContext, ToolRuntime, check_cancelled, emit, failure};
use crate::gateway_contract::GatewayError;
use base64::{Engine, engine::general_purpose::STANDARD as BASE64};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    collections::VecDeque,
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};

const RETAINED_BYTES: usize = 4 * 1024 * 1024;
const RESPONSE_BYTES: usize = 256 * 1024;
const MAX_COMMANDS: usize = 1024;

/// ConPTY can interleave console title/cursor updates inside a single Console.Write marker.
/// ConPTY 会在同次 Console.Write 标记内穿插标题或光标控制，仅在协议帧内去除这些 VT 控制。
fn protocol_payload(frame: &str) -> String {
    let mut characters = frame.chars().peekable();
    let mut payload = String::new();
    while let Some(character) = characters.next() {
        if character == '\u{1b}' {
            match characters.next() {
                Some(']') => {
                    while let Some(next) = characters.next() {
                        if next == '\u{7}' { break; }
                        if next == '\u{1b}' && characters.peek() == Some(&'\\') { characters.next(); break; }
                    }
                }
                Some('[') => {
                    for next in characters.by_ref() { if ('\u{40}'..='\u{7e}').contains(&next) { break; } }
                }
                _ => {}
            }
        } else if !character.is_ascii_whitespace() {
            payload.push(character);
        }
    }
    payload
}

/// OSC 633 introducer shared by the owned READY/DONE frames.
/// 自有 READY/DONE 帧共用的 OSC 633 起始序列。
const PROTOCOL_OPENER: &str = "\u{1b}]633;";

/// Byte offset of the BEL that ends a protocol frame, skipping escape sequences nested inside it.
/// 返回结束协议帧的 BEL 位置，跳过帧内嵌套的 OSC/CSI 转义序列；尚未收齐时返回 None。
fn protocol_frame_end(frame: &str) -> Option<usize> {
    let mut characters = frame.char_indices().peekable();
    while let Some((index, character)) = characters.next() {
        match character {
            '\u{7}' => return Some(index),
            '\u{1b}' => match characters.next() {
                Some((_, ']')) => loop {
                    match characters.next()? {
                        (_, '\u{7}') => break,
                        (_, '\u{1b}') if characters.peek().is_some_and(|(_, next)| *next == '\\') => {
                            characters.next();
                            break;
                        }
                        _ => {}
                    }
                },
                Some((_, '[')) => loop {
                    let (_, next) = characters.next()?;
                    if ('\u{40}'..='\u{7e}').contains(&next) {
                        break;
                    }
                },
                Some(_) => {}
                None => return None,
            },
            _ => {}
        }
    }
    None
}

struct CommandReceipt {
    id: String,
    fingerprint: String,
    token: String,
    status: &'static str,
    exit_code: Option<i32>,
    after: u64,
    end_cursor: Option<u64>,
    cwd: Option<String>,
    reason: Option<&'static str>,
    accepted: Option<bool>,
}

impl CommandReceipt {
    fn value(&self) -> Value {
        json!({"commandId":self.id,"status":self.status,"completed":self.status=="completed",
            "exitCode":self.exit_code,"after":self.after,"completionCursor":self.end_cursor,
            "currentWorkingDirectory":self.cwd,"reason":self.reason,"inputAccepted":self.accepted,
            "completionEvidence":if self.status=="completed"{"owned-shell-completion-marker"}else{"no-completion-proof"}})
    }
}

#[derive(Default)]
struct OutputState {
    chunks: VecDeque<(u64, String)>,
    next_cursor: u64,
    retained_bytes: usize,
    exit_code: Option<u32>,
    ended: bool,
    pending_protocol: String,
    prompt_ready: bool,
    raw_input_pending: bool,
    cwd: Option<String>,
    commands: Vec<CommandReceipt>,
    active_command: Option<String>,
}

struct TerminalOutput {
    state: Mutex<OutputState>,
    changed: tokio::sync::Notify,
    marker_prefix: String,
}

impl Default for TerminalOutput {
    fn default() -> Self {
        Self { state: Mutex::new(OutputState::default()), changed: Default::default(),
            marker_prefix: format!("\u{1b}]633;KYNXA_{};", uuid::Uuid::new_v4().simple()) }
    }
}

impl OutputState {
    fn retain(&mut self, text: String) {
        if text.is_empty() { return; }
        let cursor = self.next_cursor;
        self.next_cursor += text.len() as u64;
        self.retained_bytes += text.len();
        self.chunks.push_back((cursor, text));
        while self.retained_bytes > RETAINED_BYTES {
            if let Some((_, removed)) = self.chunks.pop_front() { self.retained_bytes -= removed.len(); }
        }
    }

    fn mark_unknown(&mut self, reason: &'static str) {
        if let Some(id) = self.active_command.take() {
            if let Some(command) = self.commands.iter_mut().find(|command| command.id == id) {
                command.status = "unknown";
                command.reason = Some(reason);
                command.end_cursor = Some(self.next_cursor);
            }
        }
    }

    fn protocol_frame(&mut self, payload: &str) -> bool {
        let parts = payload.split(':').collect::<Vec<_>>();
        let cwd = |value: &str| BASE64.decode(value).ok().and_then(|bytes| String::from_utf8(bytes).ok());
        match parts.as_slice() {
            ["READY", path] => {
                let Some(path) = cwd(path) else { return false };
                self.cwd = Some(path);
                if !self.raw_input_pending {
                    self.mark_unknown("shell-returned-without-command-completion");
                    self.prompt_ready = true;
                }
                true
            }
            ["DONE", token, code, path] => {
                let Some(path) = cwd(path) else { return false };
                let Some(command) = self.commands.iter_mut().find(|command| command.token == *token && command.status == "running") else { return false };
                let parsed = code.parse::<i32>().ok();
                if *code != "unknown" && parsed.is_none() { return false; }
                command.exit_code = parsed;
                command.accepted = Some(true);
                command.status = if parsed.is_some() { "completed" } else { "unknown" };
                command.reason = parsed.is_none().then_some("script-left-without-normal-return");
                command.cwd = Some(path.clone());
                command.end_cursor = Some(self.next_cursor);
                self.cwd = Some(path);
                self.active_command = None;
                true
            }
            _ => false,
        }
    }
}

impl TerminalOutput {
    fn append(&self, text: String) {
        if text.is_empty() {
            return;
        }
        if let Ok(mut state) = self.state.lock() {
            // Protocol tags are generated inside encoded scripts, never guessed from PS>/>> or echoed input.
            // 协议标记在编码脚本运行时生成；不把 PS>/>> 文本或输入回显当作执行证明。
            let mut pending = std::mem::take(&mut state.pending_protocol);
            pending.push_str(&text);
            // ConPTY may insert title or cursor sequences inside the frame, including their own BEL, so the
            // frame ends at the first BEL outside nested escapes and the owned identity is checked afterwards.
            // ConPTY 可能在帧内插入带 BEL 的标题或光标序列；帧在嵌套转义之外的第一个 BEL 处结束，
            // 去除这些控制序列后再核对自有标记身份，避免完成标记被截断而丢失。
            let identity = &self.marker_prefix[PROTOCOL_OPENER.len()..];
            loop {
                if let Some(start) = pending.find(PROTOCOL_OPENER) {
                    state.retain(pending[..start].to_owned());
                    pending.drain(..start);
                    if let Some(end) = protocol_frame_end(&pending[PROTOCOL_OPENER.len()..]) {
                        let end = PROTOCOL_OPENER.len() + end;
                        let payload = protocol_payload(&pending[PROTOCOL_OPENER.len()..end]);
                        let owned = payload.strip_prefix(identity);
                        if !owned.is_some_and(|frame| state.protocol_frame(frame)) {
                            state.retain(pending[..end + 1].to_owned());
                        }
                        pending.drain(..end + 1);
                    } else if pending.len() > RESPONSE_BYTES {
                        state.retain(std::mem::take(&mut pending));
                    } else { break; }
                } else {
                    let keep = (1..PROTOCOL_OPENER.len()).rev().find(|length|
                        pending.ends_with(&PROTOCOL_OPENER[..*length])).unwrap_or(0);
                    state.retain(pending[..pending.len() - keep].to_owned());
                    pending = pending[pending.len() - keep..].to_owned();
                    break;
                }
            }
            state.pending_protocol = pending;
        }
        self.changed.notify_waiters();
    }

    fn finish(&self, exit_code: Option<u32>) {
        if let Ok(mut state) = self.state.lock() {
            let pending = std::mem::take(&mut state.pending_protocol);
            state.retain(pending);
            state.ended = true;
            state.prompt_ready = false;
            state.mark_unknown("terminal-closed-or-exited-without-completion");
            if exit_code.is_some() {
                state.exit_code = exit_code;
            }
        }
        self.changed.notify_waiters();
    }

    fn read(&self, after: u64) -> Result<Value, GatewayError> {
        self.read_range(after,None)
    }

    fn read_command(&self, id: &str, after: u64) -> Result<Value, GatewayError> {
        self.read_range(after,Some(id))
    }

    fn read_range(&self, after: u64, command_id: Option<&str>) -> Result<Value, GatewayError> {
        let state = self
            .state
            .lock()
            .map_err(|_| failure("TERMINAL_STATE_FAILED", "终端输出状态不可用。", 500))?;
        let selected = command_id.map(|id|state.commands.iter().find(|command|command.id==id)
            .ok_or_else(||failure("TERMINAL_COMMAND_NOT_FOUND","当前终端不存在此命令；不会重新执行。",404))).transpose()?;
        // A receipt and its byte window are observed under one lock; later commands cannot enter an old result.
        // 回执及其字节窗口在同一锁内观测；后续命令的输出不能混入旧命令结果。
        let after = after.max(selected.map_or(0,|command|command.after));
        let limit = selected.and_then(|command|command.end_cursor).unwrap_or(state.next_cursor).min(state.next_cursor);
        let first = state
            .chunks
            .front()
            .map_or(state.next_cursor, |(cursor, _)| *cursor);
        let mut cursor = after.max(first).min(limit);
        let mut output = String::new();
        for (start, chunk) in &state.chunks {
            if *start >= limit { break; }
            let end = *start + chunk.len() as u64;
            if end <= cursor {
                continue;
            }
            let mut offset = cursor.saturating_sub(*start) as usize;
            while !chunk.is_char_boundary(offset) {
                offset += 1;
            }
            let mut count = (chunk.len() - offset).min(RESPONSE_BYTES - output.len()).min((limit-cursor) as usize);
            while !chunk.is_char_boundary(offset + count) {
                count -= 1;
            }
            output.push_str(&chunk[offset..offset + count]);
            cursor = *start + (offset + count) as u64;
            if output.len() >= RESPONSE_BYTES || cursor>=limit || offset + count < chunk.len() {
                break;
            }
        }
        Ok(
            json!({"output":output,"nextCursor":cursor,"hasMore":cursor < limit,
            "truncated":after < first,"exited":state.ended,"exitCode":state.exit_code,
            "shellReady":state.prompt_ready && !state.raw_input_pending && state.active_command.is_none(),
            "currentWorkingDirectory":state.cwd,
            "command":selected.or_else(||state.active_command.as_ref().and_then(|id|state.commands.iter().find(|command|&command.id==id))).or_else(||state.commands.last()).map(CommandReceipt::value)}),
        )
    }

    fn command(&self, id: &str) -> Result<Value, GatewayError> {
        self.state.lock().map_err(|_|failure("TERMINAL_STATE_FAILED","终端命令状态不可用。",500))?
            .commands.iter().find(|command|command.id==id).map(CommandReceipt::value)
            .ok_or_else(||failure("TERMINAL_COMMAND_NOT_FOUND","当前终端不存在此命令；不会重新执行。",404))
    }

    fn begin_command(&self, id: &str, command: &str) -> Result<Option<String>, GatewayError> {
        let fingerprint = format!("{:x}", Sha256::digest(command.as_bytes()));
        let mut state = self.state.lock().map_err(|_|failure("TERMINAL_STATE_FAILED","终端命令状态不可用。",500))?;
        if let Some(existing) = state.commands.iter().find(|existing|existing.id == id) {
            return if existing.fingerprint == fingerprint { Ok(None) } else {
                Err(failure("TERMINAL_COMMAND_ID_CONFLICT","同一 commandId 已用于不同命令；原命令不会重放。",409))
            };
        }
        if state.ended { return Err(failure("TERMINAL_CLOSED","终端已经退出。",410)); }
        if !state.prompt_ready || state.raw_input_pending || state.active_command.is_some() {
            return Err(failure("TERMINAL_BUSY","终端正在运行、尚未就绪或有用户未提交输入。请读取原会话，或明确新建独立终端；不要清空或抢写。",409));
        }
        if state.commands.len() >= MAX_COMMANDS {
            return Err(failure("TERMINAL_COMMAND_CAPACITY","此终端的去重回执已达上限。请新建终端，旧回执仍保留且不会重放。",409));
        }
        let token = uuid::Uuid::new_v4().simple().to_string();
        let after = state.next_cursor;
        let cwd = state.cwd.clone();
        state.commands.push(CommandReceipt { id:id.to_owned(),fingerprint,token:token.clone(),status:"running",
            exit_code:None,after,end_cursor:None,cwd,reason:None,accepted:None });
        state.active_command = Some(id.to_owned());
        state.prompt_ready = false;
        Ok(Some(command_input(command,&self.marker_prefix,&token)))
    }
}

fn encoded_script(script: &str) -> String {
    let bytes = script.encode_utf16().flat_map(u16::to_le_bytes).collect::<Vec<_>>();
    BASE64.encode(bytes)
}

fn shell_bootstrap(prefix: &str) -> String {
    // OSC shell-integration metadata is not painted as text, so screen wrapping cannot corrupt its identity.
    // OSC shell 集成元数据不参与屏幕文字绘制，避免回绕、重绘破坏回执身份。
    format!("function global:prompt {{ [Console]::Write('{prefix}READY:' + [Convert]::ToBase64String([Text.Encoding]::UTF8.GetBytes((Get-Location).Path)) + '\u{7}'); 'PS ' + (Get-Location).Path + '> ' }}")
}

fn command_input(command: &str, prefix: &str, token: &str) -> String {
    let payload = BASE64.encode(command.as_bytes());
    // One encoded wrapper is submitted with CR. A parser/runtime failure remains a real failed receipt.
    // 整段编码包装只用一次回车提交；语法或运行失败仍返回真实失败回执，不再陷入多行续行。
    let wrapper = format!(r#"& {{
    $kynxaCode = 'unknown'
    try {{
        $global:LASTEXITCODE = 0
        & ([ScriptBlock]::Create([Text.Encoding]::UTF8.GetString([Convert]::FromBase64String('{payload}'))))
        $kynxaSuccess = $?
        if ($global:LASTEXITCODE -ne 0) {{ $kynxaCode = [string]$global:LASTEXITCODE }}
        elseif ($kynxaSuccess) {{ $kynxaCode = '0' }} else {{ $kynxaCode = '1' }}
    }} catch {{
        $kynxaCode = '1'
        Write-Error -ErrorRecord $_ -ErrorAction Continue
    }} finally {{
        [Console]::Write('{prefix}DONE:{token}:' + $kynxaCode + ':' + [Convert]::ToBase64String([Text.Encoding]::UTF8.GetBytes((Get-Location).Path)) + '{terminator}')
    }}
}}"#,terminator='\u{7}');
    format!("& ([ScriptBlock]::Create([Text.Encoding]::Unicode.GetString([Convert]::FromBase64String('{}'))))\r", encoded_script(&wrapper))
}

pub(crate) struct TerminalSession {
    conversation_id: String,
    project_id: Option<String>,
    workdir: PathBuf,
    mount: Value,
    output: Arc<TerminalOutput>,
    closed: AtomicBool,
    #[cfg(windows)]
    native: Mutex<windows::NativeTerminal>,
}

fn dimensions(arguments: &Value) -> (i16, i16) {
    (
        arguments["cols"].as_u64().unwrap_or(100).clamp(2, 500) as i16,
        arguments["rows"].as_u64().unwrap_or(30).clamp(1, 200) as i16,
    )
}

pub(crate) async fn session_mount(context: &GatewayToolContext, id: &str) -> Result<Value, GatewayError> {
    let session = context.runtime.terminals.lock().await.get(id).cloned()
        .filter(|session|session.conversation_id == context.conversation_id && session.project_id == context.project_id)
        .ok_or_else(||failure("TERMINAL_SESSION_NOT_FOUND","当前聊天的终端不存在。",404))?;
    if session.mount.is_object() && context.config["workspaceMounts"].as_array().is_some_and(|mounts|
        !mounts.iter().any(|mount|crate::storage::workspace_mounts::same_mount(&session.mount,mount))) {
        return Err(failure("WORKSPACE_MOUNT_UNAVAILABLE","终端所在目录不在本次任务授权中。",409));
    }
    Ok(session.mount.clone())
}

pub(crate) async fn open(
    arguments: &Value,
    context: &GatewayToolContext,
) -> Result<Value, GatewayError> {
    let selected = super::gateway::select_workspace_context(context,arguments).await?;
    let context = &selected;
    #[cfg(windows)]
    {
        let (cols, rows) = dimensions(arguments);
        let conversation_id = context.conversation_id.clone();
        let project_id = context.project_id.clone();
        let workdir = context.workdir.clone();
        let mount = context.config["activeMount"].clone();
        let session = tokio::task::spawn_blocking(move || {
            let output = Arc::new(TerminalOutput::default());
            let native = windows::NativeTerminal::open(&workdir, cols, rows, output.clone())?;
            let process_id = native.process_id;
            Ok::<_, GatewayError>((
                Arc::new(TerminalSession {
                    conversation_id,
                    project_id,
                    workdir,
                    mount,
                    output,
                    closed: AtomicBool::new(false),
                    native: Mutex::new(native),
                }),
                process_id,
            ))
        })
        .await
        .map_err(|_| failure("TERMINAL_START_FAILED", "终端启动线程结束。", 500))??;
        let id = uuid::Uuid::new_v4().to_string();
        context
            .runtime
            .terminals
            .lock()
            .await
            .insert(id.clone(), session.0);
        Ok(
            json!({"sessionId":id,"processId":session.1,"cols":cols,"rows":rows,"shell":"powershell","nextCursor":0,
                "mountId":context.config["activeMountId"],"mountName":context.config["activeMount"]["name"],"initialWorkingDirectory":context.workdir}),
        )
    }
    #[cfg(not(windows))]
    {
        let _ = (arguments, context);
        Err(failure(
            "TERMINAL_PLATFORM_UNSUPPORTED",
            "内嵌终端当前支持 Windows。",
            503,
        ))
    }
}

/// Model calls share the user's registered PTY; input acceptance never certifies a command's outcome.
/// 模型复用用户已登记的伪终端；输入被接受不代表命令已经完成或成功。
pub(crate) async fn execute(
    name: &str,
    arguments: &Value,
    context: &GatewayToolContext,
) -> Result<Value, GatewayError> {
    check_cancelled(context)?;
    match name {
        "terminal.workspace.list" => {
            let sessions = context.runtime.terminals.lock().await.iter()
                .filter(|(_, session)| session.conversation_id == context.conversation_id
                    && session.project_id == context.project_id
                    && arguments["mountId"].as_str().is_none_or(|id|session.mount["id"] == id)
                    && (context.config["workspaceMounts"].as_array().is_some_and(|mounts|mounts.iter().any(|mount|crate::storage::workspace_mounts::same_mount(&session.mount,mount)))
                        || session.mount.is_null() && session.workdir == context.workdir))
                .map(|(id, session)| (id.clone(), session.clone())).collect::<Vec<_>>();
            let mut available = Vec::new();
            for (id, session) in sessions {
                if verify_scope(&session, &context.data_home).await.is_ok() {
                    let state = session.output.state.lock()
                        .map_err(|_| failure("TERMINAL_STATE_FAILED", "终端输出状态不可用。", 500))?;
                    available.push(json!({"sessionId":id,"initialWorkingDirectory":session.workdir,
                        "mountId":session.mount["id"],"mountName":session.mount["name"],
                        "shell":"powershell","exited":state.ended,"exitCode":state.exit_code,
                        "nextCursor":state.next_cursor,"closed":session.closed.load(Ordering::Acquire),
                        "currentWorkingDirectory":state.cwd,"shellReady":state.prompt_ready && !state.raw_input_pending && state.active_command.is_none(),
                        "activeCommandId":state.active_command}));
                }
            }
            available.sort_by(|left, right| left["sessionId"].as_str().cmp(&right["sessionId"].as_str()));
            Ok(json!({"sessions":available,"presentation":"workspace"}))
        }
        "terminal.workspace.open" => {
            let selected_context = super::gateway::select_workspace_context(context,arguments).await?;
            let context = &selected_context;
            verify_relationship(&context.conversation_id, context.project_id.as_deref(), &context.workdir, &context.data_home, &context.config["activeMount"]).await?;
            let selected = arguments["sessionId"].as_str().filter(|id| !id.is_empty());
            let reuse = if arguments["newSession"] == true { None } else {
                context.runtime.terminals.lock().await.iter()
                    .filter(|(id, session)| selected.is_none_or(|selected| selected == id.as_str())
                        && session.conversation_id == context.conversation_id
                        && session.project_id == context.project_id && session.workdir == context.workdir
                        && !session.closed.load(Ordering::Acquire)
                        && session.output.state.lock().is_ok_and(|state| !state.ended))
                    .map(|(id, session)| (id.clone(), session.clone())).next()
            };
            let mut opened = if let Some((id, session)) = reuse {
                verify_scope(&session, &context.data_home).await?;
                let (cols, rows) = dimensions(arguments);
                json!({"sessionId":id,"cols":cols,"rows":rows,"shell":"powershell","reused":true})
            } else {
                if selected.is_some() {
                    return Err(failure("TERMINAL_SESSION_NOT_FOUND", "当前聊天中未找到指定的可用终端。", 404));
                }
                let mut opened = open(arguments, context).await?;
                opened["reused"] = json!(false);
                opened
            };
            opened["presentation"] = json!("workspace");
            opened["initialWorkingDirectory"] = json!(context.workdir);
            opened["mountId"] = context.config["activeMountId"].clone();
            opened["mountName"] = context.config["activeMount"]["name"].clone();
            opened["currentWorkingDirectory"] = json!("not-observed");
            opened["title"] = arguments.get("title").cloned().unwrap_or_else(|| json!("终端"));
            let mut event = opened.clone();
            event["type"] = json!("workspace_terminal_open");
            event["conversationId"] = json!(context.conversation_id);
            event["workdir"] = json!(context.workdir);
            event["mountId"] = context.config["activeMountId"].clone();
            event["mountName"] = context.config["activeMount"]["name"].clone();
            emit(context, event).await;
            Ok(opened)
        }
        "terminal.workspace.read" | "terminal.workspace.write" | "terminal.workspace.run" | "terminal.workspace.close" => {
            if name != "terminal.workspace.close" {
                let session = context.runtime.terminals.lock().await.get(arguments["sessionId"].as_str().unwrap_or("")).cloned()
                    .filter(|session|session.conversation_id == context.conversation_id && session.project_id == context.project_id)
                    .ok_or_else(||failure("TERMINAL_SESSION_NOT_FOUND","当前聊天的终端不存在。",404))?;
                if session.mount.is_object() && context.config["workspaceMounts"].as_array().is_some_and(|mounts|
                    !mounts.iter().any(|mount|crate::storage::workspace_mounts::same_mount(&session.mount,mount))) {
                    return Err(failure("WORKSPACE_MOUNT_UNAVAILABLE","终端所在目录不在本次任务的授权挂载中。",409));
                }
            }
            let mut input = arguments.clone();
            input["conversationId"] = json!(context.conversation_id);
            input["action"] = json!(name.rsplit('.').next().unwrap_or(""));
            if name == "terminal.workspace.read" && input["waitMs"].is_null() {
                input["waitMs"] = json!(1000);
            }
            let mut result = action_with_cancellation(&input, &context.runtime, &context.data_home, Some(context.cancellation.clone())).await?;
            result["sessionId"] = input["sessionId"].clone();
            result["presentation"] = json!("workspace");
            Ok(result)
        }
        _ => Err(failure("TERMINAL_ACTION_INVALID", "未知的工作区终端操作。", 400)),
    }
}

pub(crate) async fn action(
    arguments: &Value,
    runtime: &Arc<ToolRuntime>,
    data_root: &Path,
) -> Result<Value, GatewayError> {
    action_with_cancellation(arguments,runtime,data_root,None).await
}

async fn action_with_cancellation(
    arguments: &Value,
    runtime: &Arc<ToolRuntime>,
    data_root: &Path,
    mut cancellation: Option<tokio::sync::watch::Receiver<bool>>,
) -> Result<Value, GatewayError> {
    let id = arguments["sessionId"].as_str().unwrap_or("");
    let conversation = arguments["conversationId"].as_str().unwrap_or("");
    let operation = arguments["action"].as_str().unwrap_or("");
    let session = runtime
        .terminals
        .lock()
        .await
        .get(id)
        .cloned()
        .filter(|session| session.conversation_id.eq_ignore_ascii_case(conversation))
        .ok_or_else(|| {
            failure(
                "TERMINAL_SESSION_NOT_FOUND",
                "当前聊天的终端会话不存在或工作范围已变化。",
                404,
            )
        })?;
    // Closing releases only this conversation's owned process; it must remain possible after a move or deletion.
    // 关闭只释放此聊天自有进程，移动或删除聊天后仍应能清理；读取及输入则必须核对当前正式归属。
    if operation != "close" {
        if let Err(error) = verify_scope(&session, data_root).await {
            return Err(error);
        }
    }
    match operation {
        "read" => {
            let after = arguments["after"].as_u64().unwrap_or(0);
            let command_id = arguments["commandId"].as_str();
            let changed = session.output.changed.notified();
            tokio::pin!(changed);
            changed.as_mut().enable();
            let current = match command_id { Some(id)=>session.output.read_command(id,after)?,None=>session.output.read(after)? };
            let wait_ms = arguments["waitMs"].as_u64().unwrap_or(15000).min(15000);
            if current["output"] != "" || current["exited"] == true || wait_ms == 0
                || command_id.is_some() && current["command"]["status"] != "running" {
                return Ok(current);
            }
            let _ = tokio::time::timeout(Duration::from_millis(wait_ms), changed).await;
            if let Err(error) = verify_scope(&session, data_root).await {
                return Err(error);
            }
            match command_id { Some(id)=>session.output.read_command(id,after),None=>session.output.read(after) }
        }
        "run" => {
            let command = arguments["command"].as_str().filter(|command|!command.trim().is_empty()
                && command.encode_utf16().count() <= 65_536 && !command.contains('\0'))
                .ok_or_else(||failure("TERMINAL_COMMAND_INVALID","命令须为非空的完整 PowerShell 脚本，且不超过 65536 字符。",400))?.to_owned();
            let command_id = arguments["commandId"].as_str().map(str::to_owned)
                .unwrap_or_else(||uuid::Uuid::new_v4().to_string());
            if command_id.is_empty() || command_id.len()>120 || !command_id.bytes().all(|byte|
                byte.is_ascii_alphanumeric() || b"-_./:".contains(&byte)) {
                return Err(failure("TERMINAL_COMMAND_INVALID","commandId 须为至多 120 字符的 ASCII 标识。",400));
            }
            let wait_ms = arguments["waitMs"].as_u64().unwrap_or(1000).min(15000);
            let ready_deadline = tokio::time::Instant::now()+Duration::from_secs(10);
            // A newly opened shell may still be reaching its first owned prompt. Never probe a busy program by typing.
            // 新终端可等待第一次自有提示标记；绝不能通过试输命令探测正在运行的程序。
            loop {
                let changed = session.output.changed.notified();
                tokio::pin!(changed);
                changed.as_mut().enable();
                let initializing = {
                    let state = session.output.state.lock().map_err(|_|failure("TERMINAL_STATE_FAILED","终端状态不可用。",500))?;
                    !state.prompt_ready && !state.raw_input_pending && !state.ended && state.active_command.is_none()
                };
                if !initializing || tokio::time::timeout_at(ready_deadline,changed).await.is_err() { break; }
            }
            if cancellation.as_ref().is_some_and(|receiver|*receiver.borrow()) {
                return Err(failure("CANCELLED","命令尚未提交，用户已停止。",499));
            }
            verify_scope(&session,data_root).await?;
            let current = session.clone();
            let submitted_id = command_id.clone();
            let reused = tokio::task::spawn_blocking(move||current.run(&submitted_id,&command)).await
                .map_err(|_|failure("TERMINAL_COMMAND_UNKNOWN","命令提交线程结束，请用原 commandId 读取，不能重放。",503))??;
            let deadline = tokio::time::Instant::now()+Duration::from_millis(wait_ms);
            let mut wait_cancelled = false;
            loop {
                let changed = session.output.changed.notified();
                tokio::pin!(changed);
                changed.as_mut().enable();
                if session.output.command(&command_id)?["status"] != "running" { break; }
                let cancelled = async {
                    if let Some(receiver) = cancellation.as_mut() { super::gateway::wait_for_cancellation(receiver).await; }
                    else { std::future::pending::<()>().await; }
                };
                tokio::select! {
                    _=cancelled=>{wait_cancelled=true;break;},
                    result=tokio::time::timeout_at(deadline,changed)=>if result.is_err(){break;}
                }
            }
            verify_scope(&session,data_root).await?;
            let mut result = session.output.read_command(&command_id,0)?;
            let receipt = result["command"].clone();
            result["shellExitCode"] = result["exitCode"].clone();
            for (field,value) in receipt.as_object().unwrap() { result[field] = value.clone(); }
            result["command"] = receipt;
            result["reused"] = json!(reused);
            result["waitCancelled"] = json!(wait_cancelled);
            result["waitTimedOut"] = json!(!wait_cancelled && result["status"]=="running");
            result["isError"] = json!(result["completed"] == true && result["exitCode"].as_i64().is_some_and(|code|code!=0));
            Ok(result)
        }
        "write" => {
            let input = arguments["input"]
                .as_str()
                .ok_or_else(|| failure("TERMINAL_INPUT_INVALID", "缺少终端输入。", 400))?;
            if input.len() > 1024 * 1024 {
                return Err(failure(
                    "TERMINAL_INPUT_INVALID",
                    "请分批发送终端输入。",
                    413,
                ));
            }
            let input = input.to_owned();
            let after = session.output.state.lock()
                .map_err(|_| failure("TERMINAL_STATE_FAILED", "终端输出状态不可用。", 500))?.next_cursor;
            tokio::task::spawn_blocking(move || session.write(&input))
                .await
                .map_err(|_| failure("TERMINAL_WRITE_FAILED", "终端输入线程结束。", 500))??;
            Ok(json!({"written":true,"inputAccepted":true,"after":after,
                "commandCompleted":null,"completionEvidence":"Read terminal output; input acceptance is not command completion."}))
        }
        "resize" => {
            let (cols, rows) = dimensions(arguments);
            tokio::task::spawn_blocking(move || session.resize(cols, rows))
                .await
                .map_err(|_| failure("TERMINAL_RESIZE_FAILED", "终端尺寸更新线程结束。", 500))??;
            Ok(json!({"cols":cols,"rows":rows}))
        }
        "close" => {
            runtime.terminals.lock().await.remove(id);
            let current = session.clone();
            tokio::task::spawn_blocking(move || current.close())
                .await
                .map_err(|_| failure("TERMINAL_CLOSE_FAILED", "终端关闭线程结束。", 500))??;
            Ok(json!({"closed":true,"command":session.output.read(u64::MAX)?["command"]}))
        }
        _ => Err(failure(
            "TERMINAL_ACTION_INVALID",
            "终端只支持读取、原始输入、结构化命令、调整尺寸或关闭。",
            400,
        )),
    }
}

async fn verify_scope(session: &TerminalSession, data_root: &Path) -> Result<(), GatewayError> {
    verify_relationship(&session.conversation_id, session.project_id.as_deref(), &session.workdir, data_root, &session.mount).await
}

async fn verify_relationship(conversation: &str, project_id: Option<&str>, workdir: &Path, data_root: &Path, mount: &Value) -> Result<(), GatewayError> {
    let root = data_root.to_owned();
    let conversation_id = conversation.to_owned();
    let relationship = tokio::task::spawn_blocking(move || {
        crate::storage::gateway::current_relationship(&root, &conversation_id)
    })
    .await
    .map_err(|_| failure("TERMINAL_SCOPE_FAILED", "终端工作范围读取线程结束。", 500))??;
    let expected_workdir = if mount.is_object() {
        relationship["mounts"].as_array().into_iter().flatten()
            .find(|current|crate::storage::workspace_mounts::same_mount(mount,current))
            .and_then(|mount|mount["path"].as_str()).map(PathBuf::from)
            .ok_or_else(||failure("TERMINAL_SCOPE_CHANGED", "终端所在挂载已移除或改变；原输出与会话仍保留。",409))?
    } else { relationship["folderPath"]
        .as_str()
        .or_else(|| relationship["workspaceRoot"].as_str())
        .filter(|folder| !folder.is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(|| data_root.join("Sandboxes").join(conversation)) };
    if relationship["projectId"].as_str() != project_id
        || expected_workdir != workdir
    {
        return Err(failure(
            "TERMINAL_SCOPE_CHANGED",
            "聊天工作范围已变化，请重新打开终端。",
            409,
        ));
    }
    Ok(())
}

impl TerminalSession {
    fn write(&self, input: &str) -> Result<(), GatewayError> {
        if self.closed.load(Ordering::Acquire) {
            return Err(failure("TERMINAL_CLOSED", "终端已关闭。", 410));
        }
        #[cfg(windows)]
        let mut native = self.native.lock().map_err(|_| failure("TERMINAL_STATE_FAILED", "终端状态不可用。", 500))?;
        {
            let mut state = self.output.state.lock().map_err(|_| failure("TERMINAL_STATE_FAILED", "终端状态不可用。", 500))?;
            state.prompt_ready = false;
            for character in input.chars() {
                state.raw_input_pending = !matches!(character,'\r'|'\u{3}');
            }
        }
        #[cfg(windows)]
        native.write(input)?;
        Ok(())
    }

    fn run(&self, id: &str, command: &str) -> Result<bool, GatewayError> {
        if self.closed.load(Ordering::Acquire) { return Err(failure("TERMINAL_CLOSED","终端已关闭。",410)); }
        #[cfg(windows)]
        let mut native = self.native.lock().map_err(|_|failure("TERMINAL_STATE_FAILED","终端状态不可用。",500))?;
        let Some(input) = self.output.begin_command(id,command)? else { return Ok(true); };
        #[cfg(windows)]
        if native.write(&input).is_err() {
            self.output.state.lock().map_err(|_|failure("TERMINAL_STATE_FAILED","终端状态不可用。",500))?
                .mark_unknown("input-write-failed-after-dispatch-start");
        } else {
            if let Ok(mut state) = self.output.state.lock() {
                if let Some(receipt) = state.commands.iter_mut().find(|receipt|receipt.id==id) { receipt.accepted=Some(true); }
            }
        }
        #[cfg(not(windows))]
        let _ = input;
        Ok(false)
    }

    fn resize(&self, cols: i16, rows: i16) -> Result<(), GatewayError> {
        #[cfg(windows)]
        self.native
            .lock()
            .map_err(|_| failure("TERMINAL_STATE_FAILED", "终端状态不可用。", 500))?
            .resize(cols, rows)?;
        Ok(())
    }

    fn close(&self) -> Result<(), GatewayError> {
        self.closed.store(true, Ordering::Release);
        #[cfg(windows)]
        self.native
            .lock()
            .map_err(|_| failure("TERMINAL_STATE_FAILED", "终端状态不可用。", 500))?
            .close()?;
        self.output.finish(None);
        Ok(())
    }
}

pub(crate) async fn shutdown(context: &GatewayToolContext) {
    let _ = shutdown_checked(context).await;
}

/// Migration waits for each owned terminal job before releasing its console and registry entry.
/// 迁移等待每个自有终端进程树退出后，再释放控制台及登记，不把清空列表当作结算成功。
pub(crate) async fn shutdown_checked(context: &GatewayToolContext) -> Result<(), GatewayError> {
    let sessions = context.runtime.terminals.lock().await.iter()
        .map(|(id, session)| (id.clone(), session.clone())).collect::<Vec<_>>();
    for (id, session) in sessions {
        #[cfg(windows)]
        {
            let current = session.clone();
            let job = tokio::task::spawn_blocking(move || {
                let mut native = current.native.lock()
                    .map_err(|_| failure("TERMINAL_STATE_FAILED", "终端状态不可用。", 500))?;
                let job = native.take_owned_job();
                Ok::<_, GatewayError>(job)
            }).await.map_err(|_| failure("TERMINAL_CLOSE_FAILED", "终端关闭线程结束。", 500))??;
            if let Some(job) = job {
                if let Err(error) = job.terminate_and_wait().await {
                    // Keep the same job available for cleanup retries without replacing the original failure.
                    // 原进程树仍由本会话持有，供后续重试结算；返回最初的退出错误。
                    let mut native = session.native.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
                    native.restore_owned_job(job);
                    return Err(error);
                }
            }
        }
        tokio::task::spawn_blocking(move || session.close()).await
            .map_err(|_| failure("TERMINAL_CLOSE_FAILED", "终端关闭线程结束。", 500))??;
        context.runtime.terminals.lock().await.remove(&id);
    }
    Ok(())
}

#[cfg(test)]
mod command_contract_tests {
    use super::*;

    fn ready(output: &TerminalOutput) {
        output.append(format!("{}READY:{}\u{7}",output.marker_prefix,BASE64.encode("D:\\fixture 中文")));
    }

    #[test]
    fn completion_marker_survives_title_and_cursor_sequences_inserted_by_conpty() {
        let output = TerminalOutput::default();
        ready(&output);
        output.begin_command("titled", "Write-Output done").unwrap().unwrap();
        let token = output.state.lock().unwrap().commands[0].token.clone();
        let cwd = BASE64.encode("D:\\fixture 中文");
        // ConPTY may put a title update (with its own BEL) and cursor toggles inside the owned frame.
        let frame = format!("{}DONE:{token}:\u{1b}]0;Windows PowerShell\u{7}\u{1b}[?25l0:{}\u{1b}[?25h{}\u{7}",
            output.marker_prefix, &cwd[..4], &cwd[4..]);
        for character in frame.chars() { output.append(character.to_string()); }
        let receipt = output.command("titled").unwrap();
        assert_eq!(receipt["completed"], true, "{receipt}");
        assert_eq!(receipt["exitCode"], 0);
        let page = output.read(0).unwrap();
        assert!(!page["output"].as_str().unwrap().contains("DONE:"), "{page}");
        // A frame owned by another terminal identity stays ordinary output and settles nothing.
        let foreign = TerminalOutput::default();
        ready(&foreign);
        foreign.begin_command("foreign", "Write-Output done").unwrap().unwrap();
        let token = foreign.state.lock().unwrap().commands[0].token.clone();
        foreign.append(format!("\u{1b}]633;KYNXA_other;DONE:{token}:0:{cwd}\u{7}"));
        assert_eq!(foreign.command("foreign").unwrap()["completed"], false);
    }

    #[test]
    fn workspace_run_receipts_require_owned_markers_and_deduplicate_exact_commands() {
        let output = TerminalOutput::default();
        let encoded_cwd=BASE64.encode("D:\\fixture 中文");
        assert_eq!(protocol_payload(&format!("READY:{}\u{1b}]0;WindowsPowerShell\u{7}\u{1b}[?25h{}",&encoded_cwd[..8],&encoded_cwd[8..])),format!("READY:{encoded_cwd}"));
        ready(&output);
        let script = "Write-Output '中文与引号 \"hello\"'\nWrite-Output 'second line'";
        let input = output.begin_command("stable-command",script).unwrap().unwrap();
        assert!(input.ends_with('\r'));
        assert!(!input.contains('\n'));
        assert!(!input.contains(&output.marker_prefix));
        assert!(output.begin_command("stable-command",script).unwrap().is_none());
        assert_eq!(output.begin_command("stable-command","different command").unwrap_err().code,"TERMINAL_COMMAND_ID_CONFLICT");
        assert_eq!(output.begin_command("other-command",script).unwrap_err().code,"TERMINAL_BUSY");
        assert_eq!(output.command("stable-command").unwrap()["status"],"running");
        let token = output.state.lock().unwrap().commands[0].token.clone();
        output.append("Actual 中文 output\r\n".into());
        let frame = format!("{}DONE:{token}:7:{}\u{7}",output.marker_prefix,BASE64.encode("D:\\fixture 中文"));
        for character in frame.chars() { output.append(character.to_string()); }
        ready(&output);
        let receipt = output.command("stable-command").unwrap();
        assert_eq!(receipt["completed"],true);
        assert_eq!(receipt["exitCode"],7);
        assert_eq!(receipt["currentWorkingDirectory"],"D:\\fixture 中文");
        assert_eq!(output.read(0).unwrap()["output"],"Actual 中文 output\r\n");
        assert!(output.begin_command("stable-command",script).unwrap().is_none());
        assert_eq!(output.state.lock().unwrap().commands.len(),1);
        output.begin_command("second-command","Write-Output 'SECOND_COMMAND_ONLY'").unwrap();
        output.append("SECOND_COMMAND_ONLY\r\n".into());
        let first = output.read_command("stable-command",0).unwrap();
        assert_eq!(first["output"],"Actual 中文 output\r\n");
        assert_eq!(first["nextCursor"],receipt["completionCursor"]);
        assert_eq!(first["hasMore"],false);
        assert!(!first["output"].as_str().unwrap().contains("SECOND_COMMAND_ONLY"));
    }

    #[test]
    fn workspace_run_preserves_partial_input_and_never_certifies_unmarked_or_closed_commands() {
        let output = TerminalOutput::default();
        ready(&output);
        output.state.lock().unwrap().raw_input_pending=true;
        ready(&output);
        assert_eq!(output.begin_command("not-submitted","Write-Output 1").unwrap_err().code,"TERMINAL_BUSY");
        assert!(output.state.lock().unwrap().commands.is_empty());
        output.state.lock().unwrap().raw_input_pending=false;
        ready(&output);
        output.begin_command("unknown-command","Start-Sleep 10").unwrap();
        output.append("PS> or >> output is not completion proof".into());
        assert_eq!(output.command("unknown-command").unwrap()["status"],"running");
        ready(&output);
        let receipt = output.command("unknown-command").unwrap();
        assert_eq!(receipt["status"],"unknown");
        assert_eq!(receipt["completed"],false);
        assert!(receipt["exitCode"].is_null());
        assert!(output.begin_command("unknown-command","Start-Sleep 10").unwrap().is_none());
        output.begin_command("closed-command","Start-Sleep 10").unwrap();
        output.finish(Some(0));
        let receipt = output.command("closed-command").unwrap();
        assert_eq!(receipt["status"],"unknown");
        assert!(receipt["exitCode"].is_null());
        assert_eq!(output.read(0).unwrap()["exitCode"],0);
    }
}

#[cfg(all(test, windows))]
mod tests {
    use super::*;

    #[tokio::test]
    async fn workspace_run_executes_multiline_in_the_same_pty_with_real_receipts_and_no_replay() {
        let (mut context,_owner)=super::super::gateway::test_context();
        let root=context.workdir.parent().unwrap().to_owned();
        crate::storage::gateway::ensure_conversation(&context.data_home,&context.conversation_id,"Structured PTY fixture").unwrap();
        context.workdir=context.data_home.join("Sandboxes").join(&context.conversation_id);
        let child=context.workdir.join("中文 folder");
        std::fs::create_dir_all(&child).unwrap();
        context.permission_mode="full".into();
        let opened=open(&json!({"cols":120,"rows":30}),&context).await.unwrap();
        let id=opened["sessionId"].clone();
        let script="Set-Location -LiteralPath '中文 folder'\nAdd-Content -LiteralPath 'once.txt' -Value '中文 ''single'' \"double\"' -Encoding UTF8\nWrite-Output 'ACTUAL_STRUCTURED_COMMAND'";
        // The default one-second observation may legitimately return running during a cold PowerShell startup.
        // PowerShell 冷启动时默认一秒观察可合法返回 running；此处等待真实完成，不能把接收回执当完成。
        let arguments=json!({"sessionId":id,"commandId":"once","command":script,"waitMs":15000,
            "reason":"Verify a whole multiline command in the visible terminal"});
        context.permission_mode="ask".into();
        let pending=super::super::gateway::execute("terminal.workspace.run",arguments.clone(),&context).await.unwrap();
        assert_eq!(pending["requiresApproval"],true);
        context.permission_mode="full".into();
        let result=super::super::gateway::execute("terminal.workspace.run",arguments.clone(),&context).await;
        let result=match result {
            Ok(result)=>result,
            Err(error)=>{
                let session=context.runtime.terminals.lock().await.get(id.as_str().unwrap()).cloned().unwrap();
                let page=session.output.read(0).unwrap();
                let state=session.output.state.lock().unwrap();
                panic!("Structured fixture failed: {error:?}; output={page}; pending={:?}; prefix={:?}",state.pending_protocol,session.output.marker_prefix);
            }
        };
        assert_eq!(result["completed"],true,"{result}");
        assert_eq!(result["exitCode"],0,"{result}");
        assert!(result["output"].as_str().unwrap().contains("ACTUAL_STRUCTURED_COMMAND"));
        assert!(result["currentWorkingDirectory"].as_str().unwrap().ends_with("中文 folder"));
        let original=std::fs::read_to_string(child.join("once.txt")).unwrap();
        let repeated=super::super::gateway::execute("terminal.workspace.run",arguments,&context).await.unwrap();
        assert_eq!(repeated["reused"],true);
        assert_eq!(std::fs::read_to_string(child.join("once.txt")).unwrap(),original);
        let mut other=context.clone();
        other.conversation_id=uuid::Uuid::new_v4().to_string();
        assert_eq!(execute("terminal.workspace.read",&json!({"sessionId":id,"commandId":"once","waitMs":0}),&other).await.unwrap_err().code,"TERMINAL_SESSION_NOT_FOUND");
        let failed=execute("terminal.workspace.run",&json!({"sessionId":id,"command":"cmd.exe /d /c exit 7","commandId":"nonzero","waitMs":15000}),&context).await.unwrap();
        assert_eq!(failed["completed"],true,"{failed}");
        assert_eq!(failed["exitCode"],7);
        let syntax=execute("terminal.workspace.run",&json!({"sessionId":id,"command":"Write-Output 'unclosed","commandId":"invalid-syntax","waitMs":15000}),&context).await.unwrap();
        assert_eq!(syntax["completed"],true,"{syntax}");
        assert_eq!(syntax["exitCode"],1);
        let waiting=execute("terminal.workspace.run",&json!({"sessionId":id,"command":"Start-Sleep -Seconds 10","commandId":"long-command","waitMs":1000}),&context).await.unwrap();
        assert_eq!(waiting["status"],"running","{waiting}");
        assert_eq!(execute("terminal.workspace.run",&json!({"sessionId":id,"command":"Write-Output 'must not execute'","commandId":"busy-other","waitMs":0}),&context).await.unwrap_err().code,"TERMINAL_BUSY");
        let observed=execute("terminal.workspace.read",&json!({"sessionId":id,"commandId":"long-command","after":waiting["nextCursor"],"waitMs":0}),&context).await.unwrap();
        assert_eq!(observed["command"]["status"],"running");
        let closed=execute("terminal.workspace.close",&json!({"sessionId":id}),&context).await.unwrap();
        assert_eq!(closed["command"]["status"],"unknown");
        assert!(closed["command"]["exitCode"].is_null());
        assert!(root.starts_with(std::env::temp_dir()));
        std::fs::remove_dir_all(root).unwrap();
    }

    #[tokio::test]
    async fn model_terminal_uses_the_selected_mount_and_keeps_it_when_default_changes() {
        use crate::storage::{gateway as data, workspace_mounts};
        let (mut context, _owner) = super::super::gateway::test_context();
        let root = context.workdir.parent().unwrap().to_owned();
        let second = root.join("secondary terminal");
        std::fs::create_dir_all(&second).unwrap();
        let project_id = uuid::Uuid::new_v4().to_string();
        let first_id = uuid::Uuid::new_v4().to_string();
        let second_id = uuid::Uuid::new_v4().to_string();
        data::ensure_conversation(&context.data_home,&context.conversation_id,"Terminal mounts").unwrap();
        let old = data::load_catalog(&context.data_home).unwrap();
        data::handle("PUT","/api/conversations/catalog",&[],&json!({"Revision":old["Revision"],"Chats":[],"Projects":[{"Id":project_id,"Name":"Terminal mounts",
            "Mounts":[{"Id":first_id,"Name":"A","Path":context.workdir,"BindingRevision":1},{"Id":second_id,"Name":"B","Path":second,"BindingRevision":1}],
            "DefaultMountId":first_id,"Chats":[{"Id":context.conversation_id,"Title":"Terminal","Messages":[
                {"Id":uuid::Uuid::new_v4().to_string(),"Role":"user","Content":"Terminal mounts fixture","Status":"completed"}]}]}]}),&context.data_home).unwrap().unwrap();
        let relation = data::relationship(&context.data_home,&context.conversation_id).unwrap();
        context.project_id = Some(project_id);
        context.permission_mode = "full".into();
        context.config["workspaceMounts"] = relation["mounts"].clone();
        context.config["defaultMountId"] = relation["defaultMountId"].clone();
        context.config["bindingSnapshot"] = workspace_mounts::snapshot(&relation);
        context.authorized_roots.push(second.clone());
        let user_opened = open(&json!({"mountId":second_id}),&context).await.unwrap();
        let id = user_opened["sessionId"].clone();
        assert_eq!(user_opened["mountId"],second_id);
        let listed = execute("terminal.workspace.list",&json!({}),&context).await.unwrap();
        assert_eq!(listed["sessions"][0]["mountId"],second_id);
        let mut catalog = data::load_catalog(&context.data_home).unwrap();
        catalog["Projects"][0]["DefaultMountId"] = json!(second_id);
        data::handle("PUT","/api/conversations/catalog",&[],&catalog,&context.data_home).unwrap().unwrap();
        // Verify a relative write rather than comparing short and long Windows path spellings.
        // 通过相对写入验证真实目录，避免 Windows 长短路径的不同拼写导致误判。
        let script = "Set-Content -LiteralPath '.cwd-verification.txt' -Value 'owned-terminal-fixture'; Write-Output ('KYNXA_' + 'CWD_READY')\r";
        let result = super::super::gateway::execute("terminal.workspace.write",json!({"sessionId":id,"input":script,"reason":"Verify the existing terminal's actual working directory"}),&context).await.unwrap();
        assert_eq!(result["inputAccepted"],true);
        tokio::time::timeout(Duration::from_secs(10),async {
            let mut cursor = result["after"].as_u64().unwrap();
            let mut text = String::new();
            loop {
                let page = execute("terminal.workspace.read",&json!({"sessionId":id,"after":cursor,"waitMs":1000}),&context).await.unwrap();
                cursor = page["nextCursor"].as_u64().unwrap();
                text.push_str(page["output"].as_str().unwrap());
                if text.contains("KYNXA_CWD_READY") { break; }
            }
        }).await.unwrap();
        assert_eq!(std::fs::read_to_string(second.join(".cwd-verification.txt")).unwrap().trim(), "owned-terminal-fixture");
        assert!(!context.workdir.join(".cwd-verification.txt").exists());
        let mut catalog = data::load_catalog(&context.data_home).unwrap();
        catalog["Projects"][0]["DefaultMountId"] = json!(first_id);
        catalog["Projects"][0]["Mounts"].as_array_mut().unwrap().retain(|mount|mount["Id"] != second_id);
        data::handle("PUT","/api/conversations/catalog",&[],&catalog,&context.data_home).unwrap().unwrap();
        assert!(super::super::gateway::execute("terminal.workspace.write",json!({"sessionId":id,"input":"echo should-not-run\r","reason":"A removed mount must reject new input"}),&context).await.is_err());
        assert!(context.runtime.terminals.lock().await.contains_key(id.as_str().unwrap()));
        execute("terminal.workspace.close",&json!({"sessionId":id,"reason":"Release the same owned terminal"}),&context).await.unwrap();
        assert!(root.starts_with(std::env::temp_dir()));
        std::fs::remove_dir_all(root).unwrap();
    }

    #[tokio::test]
    async fn model_tools_share_the_user_terminal_with_real_output_and_chat_isolation() {
        let (mut context, _owner) = super::super::gateway::test_context();
        let root = context.workdir.parent().unwrap().to_owned();
        crate::storage::gateway::ensure_conversation(&context.data_home, &context.conversation_id, "Terminal fixture").unwrap();
        context.workdir = context.data_home.join("Sandboxes").join(&context.conversation_id);
        std::fs::create_dir_all(&context.workdir).unwrap();
        let user_terminal = open(&json!({"cols":100,"rows":30}), &context).await.unwrap();
        let id = user_terminal["sessionId"].clone();
        let (events, mut received) = tokio::sync::mpsc::channel(8);
        context.events = Some(events);
        context.permission_mode = "full".into();
        let reopened = super::super::gateway::execute("terminal.workspace.open", json!({"reason":"Use the existing right terminal"}), &context).await.unwrap();
        assert_eq!(reopened["sessionId"], id);
        assert_eq!(reopened["reused"], true);
        assert_eq!(received.recv().await.unwrap()["type"], "workspace_terminal_open");
        context.permission_mode = "ask".into();
        let listed = super::super::gateway::execute("terminal.workspace.list", json!({}), &context).await.unwrap();
        assert_eq!(listed["sessions"][0]["sessionId"], id);
        let mut other_chat = context.clone();
        other_chat.conversation_id = uuid::Uuid::new_v4().to_string();
        let denied = super::super::gateway::execute("terminal.workspace.read", json!({"sessionId":id,"waitMs":0}), &other_chat).await.unwrap_err();
        assert_eq!(denied.code, "TERMINAL_SESSION_NOT_FOUND");

        let suffix = uuid::Uuid::new_v4().simple().to_string();
        let marker = format!("KYNXA_PTY_{suffix}");
        let command = format!("Write-Output ('KYNXA_' + 'PTY_{suffix}')\r");
        let pending = super::super::gateway::execute("terminal.workspace.write", json!({"sessionId":id,"input":command,"reason":"Print a test marker"}), &context).await.unwrap();
        assert_eq!(pending["requiresApproval"], true);
        context.permission_mode = "full".into();
        let written = super::super::gateway::execute("terminal.workspace.write", json!({"sessionId":id,"input":command,"reason":"Print a test marker"}), &context).await.unwrap();
        assert_eq!(written["inputAccepted"], true);
        assert!(written["commandCompleted"].is_null());
        tokio::time::timeout(Duration::from_secs(10), async {
            let mut after = written["after"].as_u64().unwrap();
            let mut output = String::new();
            loop {
                let observed = super::super::gateway::execute("terminal.workspace.read", json!({"sessionId":id,"after":after,"waitMs":1000}), &context).await.unwrap();
                output.push_str(observed["output"].as_str().unwrap());
                after = observed["nextCursor"].as_u64().unwrap();
                if output.contains(&marker) { break; }
            }
        }).await.unwrap();
        let ui_output = action(&json!({"conversationId":context.conversation_id,"sessionId":id,"action":"read","after":0,"waitMs":0}), &context.runtime, &context.data_home).await.unwrap();
        assert!(ui_output["output"].as_str().unwrap().contains(&marker));
        let closed = super::super::gateway::execute("terminal.workspace.close", json!({"sessionId":id,"reason":"Finish the test shell"}), &context).await.unwrap();
        assert_eq!(closed["closed"], true);
        assert!(context.runtime.terminals.lock().await.is_empty());
        std::fs::remove_dir_all(root).unwrap();
    }
}

#[cfg(windows)]
mod windows {
    use super::*;
    use std::{
        ffi::c_void,
        fs::File,
        io::{Read, Write},
        os::windows::{ffi::OsStrExt, io::FromRawHandle},
        path::Path,
    };
    type Handle = *mut c_void;

    #[repr(C)]
    struct Coord {
        x: i16,
        y: i16,
    }
    #[repr(C)]
    struct StartupInfo {
        cb: u32,
        reserved: *mut u16,
        desktop: *mut u16,
        title: *mut u16,
        x: u32,
        y: u32,
        x_size: u32,
        y_size: u32,
        x_chars: u32,
        y_chars: u32,
        fill: u32,
        flags: u32,
        show_window: u16,
        reserved_size: u16,
        reserved_bytes: *mut u8,
        stdin: Handle,
        stdout: Handle,
        stderr: Handle,
    }
    #[repr(C)]
    struct StartupInfoEx {
        startup: StartupInfo,
        attributes: *mut c_void,
    }
    #[repr(C)]
    #[derive(Default)]
    struct ProcessInformation {
        process: Handle,
        thread: Handle,
        process_id: u32,
        thread_id: u32,
    }

    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn CreatePipe(
            read: *mut Handle,
            write: *mut Handle,
            attributes: *const c_void,
            size: u32,
        ) -> i32;
        fn CreatePseudoConsole(
            size: Coord,
            input: Handle,
            output: Handle,
            flags: u32,
            console: *mut Handle,
        ) -> i32;
        fn ResizePseudoConsole(console: Handle, size: Coord) -> i32;
        fn ClosePseudoConsole(console: Handle);
        fn InitializeProcThreadAttributeList(
            list: *mut c_void,
            count: u32,
            flags: u32,
            size: *mut usize,
        ) -> i32;
        fn UpdateProcThreadAttribute(
            list: *mut c_void,
            flags: u32,
            attribute: usize,
            value: *mut c_void,
            size: usize,
            previous: *mut c_void,
            returned: *mut usize,
        ) -> i32;
        fn DeleteProcThreadAttributeList(list: *mut c_void);
        fn CreateProcessW(
            application: *const u16,
            command: *mut u16,
            process_attributes: *const c_void,
            thread_attributes: *const c_void,
            inherit: i32,
            flags: u32,
            environment: *const c_void,
            directory: *const u16,
            startup: *const StartupInfoEx,
            information: *mut ProcessInformation,
        ) -> i32;
        fn ResumeThread(thread: Handle) -> u32;
        fn TerminateProcess(process: Handle, code: u32) -> i32;
        fn WaitForSingleObject(handle: Handle, timeout: u32) -> u32;
        fn GetExitCodeProcess(process: Handle, code: *mut u32) -> i32;
        fn DuplicateHandle(
            source_process: Handle,
            source: Handle,
            target_process: Handle,
            target: *mut Handle,
            access: u32,
            inherit: i32,
            options: u32,
        ) -> i32;
        fn GetCurrentProcess() -> Handle;
        fn CloseHandle(handle: Handle) -> i32;
    }

    struct OwnedHandle(usize);
    impl OwnedHandle {
        fn raw(&self) -> Handle {
            self.0 as Handle
        }
        fn file(mut self) -> File {
            let raw = self.raw();
            self.0 = 0;
            unsafe { File::from_raw_handle(raw) }
        }
    }
    impl Drop for OwnedHandle {
        fn drop(&mut self) {
            if self.0 != 0 {
                unsafe {
                    CloseHandle(self.raw());
                }
            }
        }
    }
    struct PseudoConsole {
        handle: usize,
        _input: OwnedHandle,
        _output: OwnedHandle,
    }
    impl Drop for PseudoConsole {
        fn drop(&mut self) {
            if self.handle != 0 {
                unsafe {
                    ClosePseudoConsole(self.handle as Handle);
                }
            }
        }
    }

    fn os_error(code: &str, description: &str) -> GatewayError {
        failure(
            code,
            &format!("{description} {}", std::io::Error::last_os_error()),
            503,
        )
    }
    fn pipe() -> Result<(OwnedHandle, OwnedHandle), GatewayError> {
        let (mut read, mut write) = (std::ptr::null_mut(), std::ptr::null_mut());
        if unsafe { CreatePipe(&mut read, &mut write, std::ptr::null(), 0) } == 0 {
            return Err(os_error("TERMINAL_START_FAILED", "无法创建终端通信管道。"));
        }
        Ok((OwnedHandle(read as usize), OwnedHandle(write as usize)))
    }
    fn wide(value: &std::ffi::OsStr) -> Vec<u16> {
        value.encode_wide().chain(Some(0)).collect()
    }

    pub(super) struct NativeTerminal {
        pub process_id: u32,
        input: Option<File>,
        console: Arc<Mutex<Option<PseudoConsole>>>,
        process: OwnedHandle,
        job: Option<super::super::gateway_process::OwnedProcessJob>,
    }

    impl NativeTerminal {
        pub fn open(
            workdir: &Path,
            cols: i16,
            rows: i16,
            output: Arc<TerminalOutput>,
        ) -> Result<Self, GatewayError> {
            let (input_read, input_write) = pipe()?;
            let (output_read, output_write) = pipe()?;
            let mut console = std::ptr::null_mut();
            let result = unsafe {
                CreatePseudoConsole(
                    Coord { x: cols, y: rows },
                    input_read.raw(),
                    output_write.raw(),
                    0,
                    &mut console,
                )
            };
            if result < 0 {
                return Err(failure(
                    "TERMINAL_START_FAILED",
                    &format!("无法创建 ConPTY ({result:#x})。"),
                    503,
                ));
            }
            let console = PseudoConsole {
                handle: console as usize,
                _input: input_read,
                _output: output_write,
            };
            // Drain VT output on its own thread before closing the pseudoconsole; ConPTY close can otherwise block.
            // 在独立线程持续排空 VT 输出，避免关闭伪控制台时等待管道而阻塞 HTTP 或退出。
            let reader_output = output.clone();
            std::thread::Builder::new()
                .name("kynxa-terminal-output".into())
                .spawn(move || {
                    let mut reader = output_read.file();
                    let mut buffer = [0u8; 16384];
                    let mut decoder = encoding_rs::UTF_8.new_decoder_without_bom_handling();
                    loop {
                        let count = match reader.read(&mut buffer) {
                            Ok(0) | Err(_) => break,
                            Ok(count) => count,
                        };
                        let mut text = String::with_capacity(count * 3 + 4);
                        let _ = decoder.decode_to_string(&buffer[..count], &mut text, false);
                        reader_output.append(text);
                    }
                    let mut tail = String::with_capacity(16);
                    let _ = decoder.decode_to_string(&[], &mut tail, true);
                    reader_output.append(tail);
                    reader_output.finish(None);
                })
                .map_err(|_| failure("TERMINAL_START_FAILED", "无法启动终端输出读取。", 503))?;

            let mut attribute_bytes = 0usize;
            unsafe {
                InitializeProcThreadAttributeList(std::ptr::null_mut(), 1, 0, &mut attribute_bytes);
            }
            let mut attribute_storage =
                vec![0usize; attribute_bytes.div_ceil(std::mem::size_of::<usize>())];
            let attributes = attribute_storage.as_mut_ptr().cast();
            if unsafe { InitializeProcThreadAttributeList(attributes, 1, 0, &mut attribute_bytes) }
                == 0
            {
                return Err(os_error(
                    "TERMINAL_START_FAILED",
                    "无法初始化终端进程属性。",
                ));
            }
            let updated = unsafe {
                UpdateProcThreadAttribute(
                    attributes,
                    0,
                    0x00020016,
                    console.handle as Handle,
                    std::mem::size_of::<Handle>(),
                    std::ptr::null_mut(),
                    std::ptr::null_mut(),
                )
            };
            if updated == 0 {
                unsafe {
                    DeleteProcThreadAttributeList(attributes);
                }
                return Err(os_error("TERMINAL_START_FAILED", "无法绑定终端进程。"));
            }
            let executable = std::env::var_os("SystemRoot")
                .map(PathBuf::from)
                .ok_or_else(|| failure("TERMINAL_START_FAILED", "Windows 系统目录不可用。", 503))?
                .join("System32/WindowsPowerShell/v1.0/powershell.exe");
            let application = wide(executable.as_os_str());
            let bootstrap = encoded_script(&shell_bootstrap(&output.marker_prefix));
            let mut command = wide(std::ffi::OsStr::new(&format!(
                "\"{}\" -NoLogo -NoProfile -NoExit -EncodedCommand {bootstrap}", executable.display()
            )));
            let directory = wide(workdir.as_os_str());
            let mut startup: StartupInfoEx = unsafe { std::mem::zeroed() };
            startup.startup.cb = std::mem::size_of::<StartupInfoEx>() as u32;
            // Explicit null standard handles prevent inheriting redirected gateway logs/input instead of ConPTY.
            // 显式空标准句柄阻止继承网关重定向的日志和输入；子进程应连接 ConPTY 控制台。
            startup.startup.flags = 0x00000100;
            startup.attributes = attributes;
            let mut information = ProcessInformation::default();
            let created = unsafe {
                CreateProcessW(
                    application.as_ptr(),
                    command.as_mut_ptr(),
                    std::ptr::null(),
                    std::ptr::null(),
                    0,
                    0x00080000 | 0x00000004,
                    std::ptr::null(),
                    directory.as_ptr(),
                    &startup,
                    &mut information,
                )
            };
            unsafe {
                DeleteProcThreadAttributeList(attributes);
            }
            if created == 0 {
                return Err(os_error("TERMINAL_START_FAILED", "无法启动 PowerShell。"));
            }
            let process = OwnedHandle(information.process as usize);
            let thread = OwnedHandle(information.thread as usize);
            let job = match super::super::gateway_process::OwnedProcessJob::attach_pid(
                information.process_id,
            ) {
                Ok(job) => job,
                Err(error) => {
                    unsafe {
                        TerminateProcess(process.raw(), 1);
                    }
                    return Err(error);
                }
            };
            let console = Arc::new(Mutex::new(Some(console)));
            let mut observed = std::ptr::null_mut();
            let duplicate = unsafe {
                DuplicateHandle(
                    GetCurrentProcess(),
                    process.raw(),
                    GetCurrentProcess(),
                    &mut observed,
                    0,
                    0,
                    2,
                )
            };
            if duplicate == 0 {
                return Err(os_error("TERMINAL_START_FAILED", "无法观察终端进程退出。"));
            }
            let observed = OwnedHandle(observed as usize);
            let ended_console = console.clone();
            std::thread::Builder::new()
                .name("kynxa-terminal-exit".into())
                .spawn(move || {
                    unsafe {
                        WaitForSingleObject(observed.raw(), u32::MAX);
                    }
                    let mut code = 1;
                    unsafe {
                        GetExitCodeProcess(observed.raw(), &mut code);
                    }
                    if let Ok(mut state) = output.state.lock() {
                        state.exit_code = Some(code);
                    }
                    // Closing ConPTY finishes its output stream; the reader marks EOF only after draining the tail.
                    // 退出时关闭 ConPTY，读取线程排空尾部后再标记 EOF，避免末行输出被提前截走。
                    if let Ok(mut console) = ended_console.lock() {
                        console.take();
                    }
                })
                .map_err(|_| failure("TERMINAL_START_FAILED", "无法启动终端退出观察。", 503))?;
            if unsafe { ResumeThread(thread.raw()) } == u32::MAX {
                return Err(os_error("TERMINAL_START_FAILED", "无法运行终端进程。"));
            }
            Ok(Self {
                process_id: information.process_id,
                input: Some(input_write.file()),
                console,
                process,
                job: Some(job),
            })
        }

        pub fn write(&mut self, input: &str) -> Result<(), GatewayError> {
            self.input
                .as_mut()
                .ok_or_else(|| failure("TERMINAL_CLOSED", "终端已关闭。", 410))?
                .write_all(input.as_bytes())
                .map_err(|_| failure("TERMINAL_WRITE_FAILED", "终端进程无法接收输入。", 410))
        }
        pub fn resize(&mut self, cols: i16, rows: i16) -> Result<(), GatewayError> {
            let console = self
                .console
                .lock()
                .map_err(|_| failure("TERMINAL_STATE_FAILED", "终端状态不可用。", 500))?;
            let console = console
                .as_ref()
                .ok_or_else(|| failure("TERMINAL_CLOSED", "终端已关闭。", 410))?;
            let result = unsafe {
                ResizePseudoConsole(console.handle as Handle, Coord { x: cols, y: rows })
            };
            if result < 0 {
                return Err(failure("TERMINAL_RESIZE_FAILED", "无法调整终端尺寸。", 503));
            }
            Ok(())
        }
        pub fn take_owned_job(&mut self) -> Option<super::super::gateway_process::OwnedProcessJob> {
            self.job.take()
        }

        pub fn restore_owned_job(&mut self, job: super::super::gateway_process::OwnedProcessJob) {
            self.job = Some(job);
        }

        pub fn close(&mut self) -> Result<(), GatewayError> {
            self.job.take();
            self.input.take();
            if let Ok(mut console) = self.console.lock() {
                console.take();
            }
            if unsafe { WaitForSingleObject(self.process.raw(), 2000) } != 0 {
                return Err(failure("TERMINAL_CLEANUP_UNCONFIRMED", "终端进程退出尚未确认。", 503));
            }
            Ok(())
        }
    }
    impl Drop for NativeTerminal {
        fn drop(&mut self) {
            let _ = self.close();
        }
    }
}

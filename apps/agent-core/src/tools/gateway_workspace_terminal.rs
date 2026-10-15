//! Conversation-owned ConPTY sessions expose only terminal input, output and dimensions.
//! 聊天自有 ConPTY 会话只提供终端输入、输出与尺寸，工作目录由正式聊天归属解析。

use super::gateway::{GatewayToolContext, ToolRuntime, failure};
use crate::gateway_contract::GatewayError;
use serde_json::{Value, json};
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

#[derive(Default)]
struct OutputState {
    chunks: VecDeque<(u64, String)>,
    next_cursor: u64,
    retained_bytes: usize,
    exit_code: Option<u32>,
    ended: bool,
}

#[derive(Default)]
struct TerminalOutput {
    state: Mutex<OutputState>,
    changed: tokio::sync::Notify,
}

impl TerminalOutput {
    fn append(&self, text: String) {
        if text.is_empty() {
            return;
        }
        if let Ok(mut state) = self.state.lock() {
            let cursor = state.next_cursor;
            state.next_cursor += text.len() as u64;
            state.retained_bytes += text.len();
            state.chunks.push_back((cursor, text));
            while state.retained_bytes > RETAINED_BYTES {
                if let Some((_, removed)) = state.chunks.pop_front() {
                    state.retained_bytes -= removed.len();
                }
            }
        }
        self.changed.notify_waiters();
    }

    fn finish(&self, exit_code: Option<u32>) {
        if let Ok(mut state) = self.state.lock() {
            state.ended = true;
            if exit_code.is_some() {
                state.exit_code = exit_code;
            }
        }
        self.changed.notify_waiters();
    }

    fn read(&self, after: u64) -> Result<Value, GatewayError> {
        let state = self
            .state
            .lock()
            .map_err(|_| failure("TERMINAL_STATE_FAILED", "终端输出状态不可用。", 500))?;
        let first = state
            .chunks
            .front()
            .map_or(state.next_cursor, |(cursor, _)| *cursor);
        let mut cursor = after.max(first).min(state.next_cursor);
        let mut output = String::new();
        for (start, chunk) in &state.chunks {
            let end = *start + chunk.len() as u64;
            if end <= cursor {
                continue;
            }
            let mut offset = cursor.saturating_sub(*start) as usize;
            while !chunk.is_char_boundary(offset) {
                offset += 1;
            }
            let mut count = (chunk.len() - offset).min(RESPONSE_BYTES - output.len());
            while !chunk.is_char_boundary(offset + count) {
                count -= 1;
            }
            output.push_str(&chunk[offset..offset + count]);
            cursor = *start + (offset + count) as u64;
            if output.len() >= RESPONSE_BYTES || offset + count < chunk.len() {
                break;
            }
        }
        Ok(
            json!({"output":output,"nextCursor":cursor,"hasMore":cursor < state.next_cursor,
            "truncated":after < first,"exited":state.ended,"exitCode":state.exit_code}),
        )
    }
}

pub(crate) struct TerminalSession {
    conversation_id: String,
    project_id: Option<String>,
    workdir: PathBuf,
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

pub(crate) async fn open(
    arguments: &Value,
    context: &GatewayToolContext,
) -> Result<Value, GatewayError> {
    #[cfg(windows)]
    {
        let (cols, rows) = dimensions(arguments);
        let conversation_id = context.conversation_id.clone();
        let project_id = context.project_id.clone();
        let workdir = context.workdir.clone();
        let session = tokio::task::spawn_blocking(move || {
            let output = Arc::new(TerminalOutput::default());
            let native = windows::NativeTerminal::open(&workdir, cols, rows, output.clone())?;
            let process_id = native.process_id;
            Ok::<_, GatewayError>((
                Arc::new(TerminalSession {
                    conversation_id,
                    project_id,
                    workdir,
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
            json!({"sessionId":id,"processId":session.1,"cols":cols,"rows":rows,"shell":"powershell","nextCursor":0}),
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

pub(crate) async fn action(
    arguments: &Value,
    runtime: &Arc<ToolRuntime>,
    data_root: &Path,
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
            runtime.terminals.lock().await.remove(id);
            let _ = tokio::task::spawn_blocking(move || session.close()).await;
            return Err(error);
        }
    }
    match operation {
        "read" => {
            let after = arguments["after"].as_u64().unwrap_or(0);
            let changed = session.output.changed.notified();
            tokio::pin!(changed);
            changed.as_mut().enable();
            let current = session.output.read(after)?;
            if current["output"] != "" || current["exited"] == true {
                return Ok(current);
            }
            let _ = tokio::time::timeout(Duration::from_secs(15), changed).await;
            if let Err(error) = verify_scope(&session, data_root).await {
                runtime.terminals.lock().await.remove(id);
                let closing = session.clone();
                let _ = tokio::task::spawn_blocking(move || closing.close()).await;
                return Err(error);
            }
            session.output.read(after)
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
            tokio::task::spawn_blocking(move || session.write(&input))
                .await
                .map_err(|_| failure("TERMINAL_WRITE_FAILED", "终端输入线程结束。", 500))??;
            Ok(json!({"written":true}))
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
            tokio::task::spawn_blocking(move || session.close())
                .await
                .map_err(|_| failure("TERMINAL_CLOSE_FAILED", "终端关闭线程结束。", 500))??;
            Ok(json!({"closed":true}))
        }
        _ => Err(failure(
            "TERMINAL_ACTION_INVALID",
            "终端只支持读取、输入、调整尺寸或关闭。",
            400,
        )),
    }
}

async fn verify_scope(session: &TerminalSession, data_root: &Path) -> Result<(), GatewayError> {
    let root = data_root.to_owned();
    let conversation = session.conversation_id.clone();
    let relationship = tokio::task::spawn_blocking(move || {
        crate::storage::gateway::current_relationship(&root, &conversation)
    })
    .await
    .map_err(|_| failure("TERMINAL_SCOPE_FAILED", "终端工作范围读取线程结束。", 500))??;
    let workdir = relationship["folderPath"]
        .as_str()
        .or_else(|| relationship["workspaceRoot"].as_str())
        .filter(|folder| !folder.is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(|| data_root.join("Sandboxes").join(&session.conversation_id));
    if relationship["projectId"].as_str() != session.project_id.as_deref()
        || workdir != session.workdir
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
        self.native
            .lock()
            .map_err(|_| failure("TERMINAL_STATE_FAILED", "终端状态不可用。", 500))?
            .write(input)?;
        Ok(())
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
            if let Some(job) = job { job.terminate_and_wait().await?; }
        }
        tokio::task::spawn_blocking(move || session.close()).await
            .map_err(|_| failure("TERMINAL_CLOSE_FAILED", "终端关闭线程结束。", 500))??;
        context.runtime.terminals.lock().await.remove(&id);
    }
    Ok(())
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
            let mut command = wide(std::ffi::OsStr::new(&format!(
                "\"{}\" -NoLogo -NoProfile -NoExit",
                executable.display()
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

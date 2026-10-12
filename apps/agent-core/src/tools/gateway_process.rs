//! Owned ToolHost IPC and background terminal handles, without an intermediate runtime.
//! 直接管理 ToolHost 通信与后台终端句柄，不启动中间 Node 运行时。

use super::gateway::{GatewayToolContext, check_cancelled, emit, failure, native};
use crate::gateway_contract::GatewayError;
use serde_json::{Value, json};
use std::path::Path;
use std::process::Stdio;
use std::sync::Arc;
use std::time::Duration;
use tokio::io::{AsyncBufReadExt, AsyncRead, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, Command};
use tokio::sync::{Mutex, watch};

/// Windows closes the entire owned process tree if the gateway crashes or releases this handle.
/// 网关崩溃或释放句柄时，Windows 自动关闭此自有进程树，避免只清理父进程。
pub(crate) struct OwnedProcessJob(usize);

/// A checked, point-in-time sample of this job's process tree, not a reservation or peak.
/// 已核验的自有作业进程树时点采样，不代表资源预约或占用峰值。
pub(crate) struct OwnedProcessMemory {
    pub private_bytes: u64,
    pub working_set_bytes: u64,
    pub processes: usize,
}

#[cfg(windows)]
#[repr(C)]
#[derive(Default)]
struct ProcessMemoryCounters {
    size: u32,
    faults: u32,
    counters: [usize; 9],
}
#[cfg(windows)]
#[repr(C)]
#[derive(Default)]
struct JobLimits {
    process_time: i64,
    job_time: i64,
    flags: u32,
    min_working_set: usize,
    max_working_set: usize,
    active_processes: u32,
    affinity: usize,
    priority: u32,
    scheduling: u32,
    io: [u64; 6],
    process_memory: usize,
    job_memory: usize,
    peak_process: usize,
    peak_job: usize,
}
#[cfg(windows)]
#[repr(C)]
#[derive(Default)]
struct JobAccounting {
    total_user_time: i64,
    total_kernel_time: i64,
    period_user_time: i64,
    period_kernel_time: i64,
    page_faults: u32,
    total_processes: u32,
    active_processes: u32,
    terminated_processes: u32,
}
#[cfg(windows)]
#[link(name = "kernel32")]
unsafe extern "system" {
    fn CreateJobObjectW(
        attributes: *const std::ffi::c_void,
        name: *const u16,
    ) -> *mut std::ffi::c_void;
    fn SetInformationJobObject(
        job: *mut std::ffi::c_void,
        class: i32,
        information: *const std::ffi::c_void,
        size: u32,
    ) -> i32;
    fn OpenProcess(access: u32, inherit: i32, pid: u32) -> *mut std::ffi::c_void;
    fn AssignProcessToJobObject(job: *mut std::ffi::c_void, process: *mut std::ffi::c_void) -> i32;
    fn QueryInformationJobObject(
        job: *mut std::ffi::c_void,
        class: i32,
        information: *mut std::ffi::c_void,
        size: u32,
        returned: *mut u32,
    ) -> i32;
    fn TerminateJobObject(job: *mut std::ffi::c_void, exit_code: u32) -> i32;
    fn CloseHandle(handle: *mut std::ffi::c_void) -> i32;
    fn IsProcessInJob(
        process: *mut std::ffi::c_void,
        job: *mut std::ffi::c_void,
        result: *mut i32,
    ) -> i32;
    fn K32GetProcessMemoryInfo(
        process: *mut std::ffi::c_void,
        counters: *mut ProcessMemoryCounters,
        size: u32,
    ) -> i32;
}
impl OwnedProcessJob {
    /// Read only this job's checked process handles; private commit and working set are distinct point samples.
    /// 只读取本作业已核验的进程句柄，私有提交和工作集分别返回；这是时点采样，不是内存峰值。
    pub(crate) fn memory_usage(&self) -> Result<OwnedProcessMemory, GatewayError> {
        #[cfg(windows)]
        unsafe {
            let job = self.0 as *mut std::ffi::c_void;
            let mut capacity = 32usize;
            let ids = loop {
                let mut buffer = vec![0usize; capacity + 2];
                let success = QueryInformationJobObject(
                    job,
                    3,
                    buffer.as_mut_ptr().cast(),
                    (8 + capacity * std::mem::size_of::<usize>()) as u32,
                    std::ptr::null_mut(),
                );
                let header = buffer.as_ptr().cast::<u32>();
                let count = header.add(1).read_unaligned() as usize;
                let assigned = header.read_unaligned() as usize;
                if success != 0 && count <= capacity && assigned <= count {
                    let pointer = (buffer.as_ptr().cast::<u8>()).add(8).cast::<usize>();
                    break (0..count)
                        .map(|index| pointer.add(index).read_unaligned())
                        .collect::<Vec<_>>();
                }
                if capacity >= 2048 {
                    return Err(failure(
                        "TOOL_PROCESS_MEMORY_UNKNOWN",
                        "自有进程树内存采样不完整。",
                        503,
                    ));
                }
                capacity *= 2;
            };
            let mut sample = OwnedProcessMemory {
                private_bytes: 0,
                working_set_bytes: 0,
                processes: ids.len(),
            };
            for id in ids {
                let process = OpenProcess(0x1010, 0, id as u32);
                if process.is_null() {
                    return Err(failure(
                        "TOOL_PROCESS_MEMORY_UNKNOWN",
                        "自有进程内存采样不可用。",
                        503,
                    ));
                }
                let mut belongs = 0;
                let mut memory = ProcessMemoryCounters {
                    size: std::mem::size_of::<ProcessMemoryCounters>() as u32,
                    ..Default::default()
                };
                let valid = IsProcessInJob(process, job, &mut belongs) != 0
                    && belongs != 0
                    && K32GetProcessMemoryInfo(
                        process,
                        &mut memory,
                        std::mem::size_of::<ProcessMemoryCounters>() as u32,
                    ) != 0
                    && IsProcessInJob(process, job, &mut belongs) != 0
                    && belongs != 0;
                CloseHandle(process);
                if !valid {
                    return Err(failure(
                        "TOOL_PROCESS_MEMORY_UNKNOWN",
                        "自有进程身份或内存采样已改变。",
                        503,
                    ));
                }
                sample.private_bytes = sample
                    .private_bytes
                    .saturating_add(memory.counters[8] as u64);
                sample.working_set_bytes = sample
                    .working_set_bytes
                    .saturating_add(memory.counters[1] as u64);
            }
            Ok(sample)
        }
        #[cfg(not(windows))]
        {
            Err(failure(
                "TOOLHOST_PLATFORM_UNSUPPORTED",
                "自有进程内存采样仅支持 Windows。",
                503,
            ))
        }
    }
    /// A launcher may exit successfully after handing off to an owned child; observe the job rather than one PID.
    /// 启动器可能把运行交给自有子进程后正常退出；存活检查针对整个作业，不能仅检查首个 PID。
    pub(crate) fn has_running_processes(&self) -> Result<bool, GatewayError> {
        #[cfg(windows)]
        unsafe {
            let mut accounting = JobAccounting::default();
            if QueryInformationJobObject(
                self.0 as *mut std::ffi::c_void,
                1,
                &mut accounting as *mut _ as *mut std::ffi::c_void,
                std::mem::size_of::<JobAccounting>() as u32,
                std::ptr::null_mut(),
            ) == 0
            {
                return Err(failure(
                    "TOOL_PROCESS_STATE_UNKNOWN",
                    "无法核验自有进程树状态。",
                    503,
                ));
            }
            Ok(accounting.active_processes != 0)
        }
        #[cfg(not(windows))]
        {
            Err(failure(
                "TOOLHOST_PLATFORM_UNSUPPORTED",
                "自有进程树仅支持 Windows。",
                503,
            ))
        }
    }

    pub(crate) async fn terminate_and_wait(&self) -> Result<(), GatewayError> {
        #[cfg(windows)]
        unsafe {
            if TerminateJobObject(self.0 as *mut std::ffi::c_void, 1) == 0
                && self.has_running_processes()?
            {
                return Err(failure(
                    "TOOL_PROCESS_CLEANUP_FAILED",
                    "自有进程树退出未确认。",
                    503,
                ));
            }
        }
        let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
        while self.has_running_processes()? {
            if tokio::time::Instant::now() >= deadline {
                return Err(failure(
                    "TOOL_PROCESS_CLEANUP_FAILED",
                    "自有进程树仍未退出。",
                    503,
                ));
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
        Ok(())
    }

    pub(crate) fn attach(child: &Child) -> Result<Self, GatewayError> {
        Self::attach_with_policy(child, false)
    }

    /// Only the approved GUI launch helper may create an explicitly independent application.
    /// 仅已批准的图形应用启动助手允许显式创建独立应用；终端与 MCP 进程树仍不能脱离。
    fn attach_for_launch(child: &Child) -> Result<Self, GatewayError> {
        Self::attach_with_policy(child, true)
    }

    fn attach_with_policy(child: &Child, allow_breakaway: bool) -> Result<Self, GatewayError> {
        #[cfg(windows)]
        unsafe {
            let pid = child
                .id()
                .ok_or_else(|| failure("TOOL_PROCESS_EXITED", "进程在登记前退出。", 503))?;
            let job = CreateJobObjectW(std::ptr::null(), std::ptr::null());
            if job.is_null() {
                return Err(failure(
                    "TOOL_PROCESS_ISOLATION_FAILED",
                    "无法创建自有进程生命周期作业。",
                    503,
                ));
            }
            let limits = JobLimits {
                flags: 0x2000 | if allow_breakaway { 0x0800 } else { 0 },
                ..Default::default()
            };
            if SetInformationJobObject(
                job,
                9,
                &limits as *const _ as *const std::ffi::c_void,
                std::mem::size_of::<JobLimits>() as u32,
            ) == 0
            {
                CloseHandle(job);
                return Err(failure(
                    "TOOL_PROCESS_ISOLATION_FAILED",
                    "无法设置退出时清理进程树。",
                    503,
                ));
            }
            let process = OpenProcess(0x0101, 0, pid);
            let success = !process.is_null() && AssignProcessToJobObject(job, process) != 0;
            if !process.is_null() {
                CloseHandle(process);
            }
            if !success {
                CloseHandle(job);
                return Err(failure(
                    "TOOL_PROCESS_ISOLATION_FAILED",
                    "无法登记自有进程树；未继续派发操作。",
                    503,
                ));
            }
            Ok(Self(job as usize))
        }
        #[cfg(not(windows))]
        {
            let _ = (child, allow_breakaway);
            Err(failure(
                "TOOLHOST_PLATFORM_UNSUPPORTED",
                "本工具执行器仅支持 Windows。",
                503,
            ))
        }
    }
}
impl Drop for OwnedProcessJob {
    fn drop(&mut self) {
        #[cfg(windows)]
        unsafe {
            if self.0 != 0 {
                CloseHandle(self.0 as *mut std::ffi::c_void);
            }
        }
    }
}

/// Keep a background terminal's cancellation, receipt and completion tied to its conversation.
/// 将后台终端的取消、回执和完成通知绑定到所属聊天，避免跨聊天复用任务。
#[derive(Clone)]
pub(crate) struct HostJob {
    pub(crate) conversation_id: String,
    cancel: watch::Sender<bool>,
    result: Arc<Mutex<Option<Value>>>,
    finished: watch::Receiver<bool>,
}

pub(crate) fn hide(command: &mut Command) {
    #[cfg(windows)]
    command.creation_flags(0x08000000);
}

/// Check the limit before allocation, including a peer that never sends a newline.
/// 即使对端永远不发换行，也在分配之前检查上限，避免先无限读取再验大小。
pub(crate) async fn bounded_line<T: AsyncRead + Unpin>(
    reader: &mut BufReader<T>,
    maximum_bytes: usize,
) -> std::io::Result<Option<String>> {
    let mut frame = Vec::new();
    loop {
        let available = reader.fill_buf().await?;
        if available.is_empty() {
            return if frame.is_empty() {
                Ok(None)
            } else {
                Err(std::io::Error::new(
                    std::io::ErrorKind::UnexpectedEof,
                    "Incomplete native frame",
                ))
            };
        }
        let newline = available.iter().position(|byte| *byte == b'\n');
        let consumed = newline.map_or(available.len(), |index| index + 1);
        if frame.len() + consumed > maximum_bytes {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "Native frame exceeded limit",
            ));
        }
        frame.extend_from_slice(&available[..consumed]);
        reader.consume(consumed);
        if newline.is_some() {
            while frame
                .last()
                .is_some_and(|byte| [b'\r', b'\n'].contains(byte))
            {
                frame.pop();
            }
            return String::from_utf8(frame).map(Some).map_err(|_| {
                std::io::Error::new(std::io::ErrorKind::InvalidData, "Native frame is not UTF-8")
            });
        }
    }
}

/// Only the child created by this invocation may be terminated, never a process name.
/// 仅终止本次创建且仍持有句柄的子进程树，绝不按通用进程名杀进程。
pub(crate) async fn terminate(child: &mut tokio::process::Child) {
    #[cfg(windows)]
    if let Some(pid) = child.id() {
        if child.try_wait().ok().flatten().is_none() {
            if let Some(system_root) = std::env::var_os("SystemRoot") {
                let executable = Path::new(&system_root).join("System32/taskkill.exe");
                let mut command = Command::new(executable);
                hide(&mut command);
                let _ = command
                    .args(["/PID", &pid.to_string(), "/T", "/F"])
                    .stdout(Stdio::null())
                    .stderr(Stdio::null())
                    .status()
                    .await;
            }
        }
    }
    let _ = child.kill().await;
    let _ = child.wait().await;
}

/// Dispatch one owned helper, retain streamed facts, then release its job and resource lease.
/// 派发一个自有助手、保留流式事实，最后释放作业及资源租约；缺失最终回执保留未知状态。
async fn invoke(
    request: Value,
    timeout_ms: u64,
    ctx: &GatewayToolContext,
    job_id: Option<&str>,
) -> Result<Value, GatewayError> {
    check_cancelled(ctx)?;
    if !ctx.toolhost_exe.is_file() {
        return Err(failure(
            "TOOLHOST_UNAVAILABLE",
            "原生 ToolHost 未随包提供。",
            503,
        ));
    }
    let resource_plan = native(
        ctx,
        "runner.resourcePlan",
        json!({
            "kind": if request["operation"] == "desktop" { "desktop" } else { "host" },
            "request": request
        }),
    )
    .await?;
    let lease = if resource_plan["required"] == true {
        if let Some(service) = crate::resources::gateway::global() {
            let mut requirements = resource_plan["requirements"].clone();
            requirements["taskId"] = json!(uuid::Uuid::new_v4().to_string());
            requirements["workspaceId"] =
                json!(ctx.project_id.as_deref().unwrap_or(&ctx.conversation_id));
            requirements["kind"] = json!(if request["operation"] == "host_terminal_job" {
                "background"
            } else {
                "foreground"
            });
            requirements["workload"] = json!("compute");
            requirements["waitMs"] = json!(30000);
            requirements["ttlMs"] = json!(30000);
            Some(
                service
                    .acquire(requirements, ctx.cancellation.clone())
                    .await?,
            )
        } else {
            None
        }
    } else {
        None
    };
    let mut command = Command::new(&ctx.toolhost_exe);
    hide(&mut command);
    let mut child = command
        .current_dir(&ctx.workdir)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .map_err(|_| failure("TOOLHOST_START_FAILED", "原生工具助手启动失败。", 503))?;
    let owned_job = if request["operation"] == "desktop" && request["action"] == "launch" {
        OwnedProcessJob::attach_for_launch(&child)
    } else {
        OwnedProcessJob::attach(&child)
    };
    let _owned = match owned_job {
        Ok(job) => job,
        Err(error) => {
            terminate(&mut child).await;
            return Err(error);
        }
    };
    let mut stdin = child
        .stdin
        .take()
        .ok_or_else(|| failure("TOOLHOST_PIPE_FAILED", "助手输入管道不可用。", 500))?;
    let mut stdout = BufReader::new(child.stdout.take().unwrap());
    let stderr = child.stderr.take().unwrap();
    let diagnostic = tokio::spawn(async move {
        let mut bytes = Vec::new();
        let _ = stderr.take(64 * 1024).read_to_end(&mut bytes).await;
    });
    let encoded = serde_json::to_vec(&request)
        .map_err(|_| failure("INVALID_TOOL_ARGUMENTS", "原生请求编码失败。", 400))?;
    if encoded.len() > 128 * 1024 {
        terminate(&mut child).await;
        diagnostic.abort();
        return Err(failure(
            "TOOL_REQUEST_TOO_LARGE",
            "原生请求超过传输限额。",
            400,
        ));
    }
    if stdin.write_all(&encoded).await.is_err()
        || stdin.write_all(b"\n").await.is_err()
        || stdin.flush().await.is_err()
    {
        terminate(&mut child).await;
        diagnostic.abort();
        return Err(failure(
            "TOOLHOST_EXECUTION_UNKNOWN",
            "原生助手请求发送未确认；可能已有部分字节提交，不能直接重放。",
            502,
        ));
    }
    let deadline = tokio::time::sleep(Duration::from_millis(timeout_ms));
    tokio::pin!(deadline);
    let mut cancellation = ctx.cancellation.clone();
    let mut receipt = None;
    let mut total_bytes = 0usize;
    loop {
        let next = tokio::select! {
            line = bounded_line(&mut stdout, 32 * 1024 * 1024) => line.ok().flatten(),
            _ = &mut deadline => None,
            _ = cancellation.changed() => {
                if *cancellation.borrow_and_update() {
                    None
                } else {
                    continue
                }
            },
        };
        let Some(line) = next else {
            break;
        };
        total_bytes = total_bytes.saturating_add(line.len());
        if total_bytes > 64 * 1024 * 1024 || line.len() > 32 * 1024 * 1024 {
            break;
        }
        let Ok(value) = serde_json::from_str::<Value>(&line) else {
            break;
        };
        if value["event"].is_string() {
            if let Some(id) = job_id {
                if value["event"] == "host_terminal_started" {
                    native(
                        ctx,
                        "host.started",
                        json!({"jobId":id,"processId":value["processId"]}),
                    )
                    .await?;
                }
                if value["event"] == "host_terminal_output" {
                    let units = value["delta"]
                        .as_str()
                        .or_else(|| value["text"].as_str())
                        .unwrap_or("")
                        .encode_utf16()
                        .collect::<Vec<_>>();
                    native(
                        ctx,
                        "host.append",
                        json!({"jobId":id,"units":units,"replace":value["replace"]}),
                    )
                    .await?;
                }
            }
            if value["event"] == "host_terminal_output" {
                let text = value["delta"]
                    .as_str()
                    .or_else(|| value["text"].as_str())
                    .unwrap_or("");
                emit(
                    ctx,
                    json!({
                        "type": "terminal_output",
                        "terminal": {
                            "toolCallId": ctx.config["activeToolCallId"],
                            "sequence": value["sequence"].as_u64().unwrap_or(1).max(1),
                            "stream": value["stream"].as_str().unwrap_or("stdout"),
                            "text": text,
                            "replace": value["replace"].as_bool().unwrap_or(false)
                        }
                    }),
                )
                .await;
            }
        } else {
            receipt = Some(value);
            break;
        }
    }
    if receipt.is_none() {
        let _ = stdin.write_all(b"cancel\n").await;
        let _ = stdin.flush().await;
        // Allow the helper to close its Win32 job and report partial effects before hard cleanup.
        // 先让助手关闭 Win32 作业并报告部分效果，再执行受控硬清理；取消不会伪装成未执行。
        if let Ok(Ok(Some(line))) = tokio::time::timeout(
            Duration::from_secs(3),
            bounded_line(&mut stdout, 32 * 1024 * 1024),
        )
        .await
        {
            if let Ok(value) = serde_json::from_str::<Value>(&line) {
                if value["event"].is_null() {
                    receipt = Some(value);
                }
            }
        }
        terminate(&mut child).await;
    } else if tokio::time::timeout(Duration::from_secs(3), child.wait())
        .await
        .is_err()
    {
        terminate(&mut child).await;
    }
    diagnostic.abort();
    if let Some(lease) = lease {
        lease.release().await;
    }
    Ok(receipt.unwrap_or_else(|| {
        json!({
            "protocolVersion": 1,
            "boundary": if request["operation"] == "desktop" { "host-desktop" } else { "host-terminal" },
            "completed": false,
            "outcome": "unknown",
            "status": "unknown",
            "isError": true,
            "code": "TOOL_EXECUTION_UNKNOWN",
            "error": {
                "code": "TOOL_EXECUTION_UNKNOWN",
                "message": "请求已提交，但没有取得最终回执；请查询实际状态，不要重放操作。"
            }
        })
    }))
}

/// Validate host policy and working-directory identity before requesting a native terminal receipt.
/// 请求原生终端回执前核对宿主策略与工作目录身份，不把终端执行迁到沙箱或替换错误类别。
async fn host_run(
    mut input: Value,
    ctx: &GatewayToolContext,
    job_id: Option<&str>,
) -> Result<Value, GatewayError> {
    let background = job_id.is_some();
    let forbidden = super::intent::calculate(
        "tools.foregroundForbidden",
        &json!({
            "message": ctx.config["trustedUserText"],
            "target": { "kind": "terminal", "applicationName": input["shell"] }
        }),
    )
    .map_err(|message| failure("TOOL_INTENT_FAILED", &message, 500))?;
    if input["visible"] == true && forbidden == true {
        return Err(failure(
            "DESKTOP_FOREGROUND_FORBIDDEN",
            "用户要求后台终端，未打开可见窗口。",
            403,
        ));
    }
    input["cwd"] = json!(
        input["cwd"]
            .as_str()
            .unwrap_or(ctx.workdir.to_str().unwrap_or(""))
    );
    input["timeoutMs"] =
        json!(
            input["timeoutMs"]
                .as_u64()
                .unwrap_or(if background { 1_800_000 } else { 120_000 })
        );
    input["visible"] = json!(input["visible"].as_bool().unwrap_or(false));
    input["backgroundJob"] = json!(background);
    let requested_cwd = Path::new(input["cwd"].as_str().unwrap_or(""));
    if !requested_cwd.is_absolute() || !requested_cwd.is_dir() {
        return Err(failure(
            "HOST_TERMINAL_INVALID_WORKSPACE",
            "终端目录不存在。",
            400,
        ));
    }
    // Resolve the approved directory before dispatch so Windows short-name expansion cannot invalidate its receipt.
    // 派发前解析已批准的目录，避免 Windows 短名称展开导致同一真实目录的回执身份误判。
    let resolved_cwd = super::service_filesystem::canonical(requested_cwd, false)
        .map_err(|error| failure(&error.code, &error.message, error.status_code))?;
    input["cwd"] = json!(super::local_files::path_text(&resolved_cwd));
    let cwd = Path::new(input["cwd"].as_str().unwrap());
    let plan = native(
        ctx,
        "runner.hostRequest",
        json!({"input":input,"cwdAbsolute":cwd.is_absolute()}),
    )
    .await?;
    let capabilities = invoke(
        json!({"operation":"host_terminal_capabilities"}),
        10_000,
        ctx,
        None,
    )
    .await?;
    native(
        ctx,
        "runner.hostRequiredCapabilities",
        json!({
            "value": capabilities,
            "visible": input["visible"],
            "backgroundJob": background,
            "timeoutMs": input["timeoutMs"]
        }),
    )
    .await?;
    let mut request = input.clone();
    request.as_object_mut().unwrap().remove("reason");
    request["operation"] = plan["operation"].clone();
    if input["visible"] == true {
        request["keepOpenMs"] = plan["holdMs"].clone();
    }
    let value = invoke(
        request,
        plan["maximumWaitMs"].as_u64().unwrap_or(130_000),
        ctx,
        job_id,
    )
    .await?;
    host_receipt(&input, &plan["holdMs"], value, ctx).await
}

/// Retain terminal output and identity while applying only the runner's verified state overrides.
/// 保留终端输出与身份，仅合并执行器已核验的状态修正；后台完成记录使用同一完整回执。
async fn host_receipt(
    parameters: &Value,
    hold_ms: &Value,
    mut value: Value,
    ctx: &GatewayToolContext,
) -> Result<Value, GatewayError> {
    let plan = native(
        ctx,
        "runner.hostResult",
        json!({
            "value": value,
            "shell": parameters["shell"],
            "cwd": parameters["cwd"],
            "visible": parameters["visible"],
            "holdMs": hold_ms
        }),
    )
    .await?;
    if let Some(overrides) = plan["overrides"].as_object() {
        for (field, field_value) in overrides {
            value[field] = field_value.clone();
        }
    }
    let mut receipt = json!({"value":value,"isError":plan["isError"]});
    for field in ["status", "code"] {
        if let Some(field_value) = plan.get(field) {
            receipt[field] = field_value.clone();
        }
    }
    Ok(receipt)
}

/// Route terminal and desktop tools without duplicating the native runner's policy or job state.
/// 接线终端与桌面工具，复用原生执行器的策略和任务状态，不另建执行账本。
pub(crate) async fn execute(
    name: &str,
    mut input: Value,
    ctx: &GatewayToolContext,
) -> Result<Value, GatewayError> {
    if name == "terminal.host.run" {
        return host_run(input, ctx, None).await;
    }
    if name == "terminal.host.start" {
        let plan = native(
            ctx,
            "host.start",
            json!({
                "context": { "conversationId": ctx.conversation_id },
                "request": input,
                "startedAt": chrono::Utc::now().to_rfc3339()
            }),
        )
        .await?;
        let id = plan["jobId"].as_str().unwrap_or("").to_owned();
        for evicted in plan["evicted"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(Value::as_str)
        {
            ctx.runtime.jobs.lock().await.remove(evicted);
        }
        let (cancel, receiver) = watch::channel(false);
        let (finished_tx, finished) = watch::channel(false);
        let job = HostJob {
            conversation_id: ctx.conversation_id.clone(),
            cancel,
            result: Arc::new(Mutex::new(None)),
            finished,
        };
        ctx.runtime
            .jobs
            .lock()
            .await
            .insert(id.clone(), job.clone());
        let mut owned = ctx.clone();
        owned.cancellation = receiver;
        let task_id = id.clone();
        tokio::spawn(async move {
            let value = host_run(input, &owned, Some(&task_id))
                .await
                .unwrap_or_else(|error| {
                    json!({
                        "isError": true,
                        "code": error.code,
                        "content": error.message,
                        "status": "unknown"
                    })
                });
            let _ = native(
                &owned,
                "host.complete",
                json!({"jobId":task_id,"result":value}),
            )
            .await;
            let _ = native(
                &owned,
                "host.patch",
                json!({"jobId":task_id,"fields":{"finishedAt":chrono::Utc::now().to_rfc3339()}}),
            )
            .await;
            *job.result.lock().await = Some(value);
            let _ = finished_tx.send(true);
        });
        let deadline = tokio::time::Instant::now() + Duration::from_secs(15);
        loop {
            if *ctx.cancellation.borrow() {
                if let Some(job) = ctx.runtime.jobs.lock().await.get(&id) {
                    let _ = job.cancel.send(true);
                }
                return Ok(unknown_job(
                    &id,
                    "后台任务启动等待已取消；已请求终止自有进程。",
                ));
            }
            let state = native(
                ctx,
                "host.view",
                json!({"context":{"conversationId":ctx.conversation_id},"jobId":id}),
            )
            .await?;
            if state["status"] != "starting" {
                return Ok(if state["status"] == "unknown" {
                    unknown_job(
                        &id,
                        "后台任务派发结果未知；请读取该自有任务状态，不能直接重放。",
                    )
                } else {
                    state
                });
            }
            if tokio::time::Instant::now() >= deadline {
                if let Some(job) = ctx.runtime.jobs.lock().await.get(&id) {
                    let _ = job.cancel.send(true);
                }
                return Ok(unknown_job(
                    &id,
                    "后台任务尚未确认启动；已请求取消，不会重放。",
                ));
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
    }
    if name == "terminal.host.read" || name == "terminal.host.stop" {
        let id = input["jobId"].as_str().unwrap_or("");
        let job = ctx
            .runtime
            .jobs
            .lock()
            .await
            .get(id)
            .cloned()
            .filter(|job| job.conversation_id == ctx.conversation_id)
            .ok_or_else(|| failure("HOST_TERMINAL_JOB_NOT_FOUND", "当前聊天中没有此任务。", 404))?;
        if name == "terminal.host.stop" {
            let _ = job.cancel.send(true);
            let mut finished = job.finished.clone();
            if !*finished.borrow() {
                let _ = tokio::time::timeout(Duration::from_secs(8), finished.changed()).await;
            }
        }
        input["context"] = json!({"conversationId":ctx.conversation_id});
        let mut value = native(
            ctx,
            if name.ends_with("read") {
                "host.read"
            } else {
                "host.view"
            },
            input,
        )
        .await?;
        if let Some(units) = value["outputUnits"].as_array() {
            let units = units
                .iter()
                .filter_map(Value::as_u64)
                .map(|unit| unit as u16)
                .collect::<Vec<_>>();
            value["output"] = json!(String::from_utf16_lossy(&units));
            value.as_object_mut().unwrap().remove("outputUnits");
        }
        return Ok(value);
    }
    if name == "terminal.run" {
        let command = input["command"].as_str().unwrap_or("");
        let use_node = ["node", "node.exe"].contains(&command);
        if !["cmd", "cmd.exe", "node", "node.exe"].contains(&command) {
            return Err(failure(
                "SANDBOX_COMMAND_UNSUPPORTED",
                "沙箱仅支持原生 CMD 与随包私有 Node。",
                400,
            ));
        }
        let private_node = ctx.config["officialToolsRoot"]
            .as_str()
            .and_then(|root| std::path::Path::new(root).parent())
            .map(|root| root.join("runtime/mcp/node/node.exe"));
        if use_node && !private_node.as_ref().is_some_and(|path| path.is_file()) {
            return Err(failure(
                "NODE_RUNTIME_UNAVAILABLE",
                "随包私有 Node 尚未准备；未转到宿主执行或临时下载。",
                503,
            ));
        }
        let caps = invoke(json!({"operation":"capabilities"}), 10_000, ctx, None).await?;
        if caps["available"] != true
            || caps["failClosed"] != true
            || caps["checksChildToken"] != true
        {
            return Err(failure(
                "SANDBOX_UNAVAILABLE",
                "已验证的 AppContainer 不可用；未退到宿主执行。",
                503,
            ));
        }
        input["operation"] = json!("run");
        input["workspaceRoot"] = json!(ctx.workdir);
        // Node selection is application-owned, never accepted from the tool arguments or system PATH.
        // Node 路径由应用拥有，不采信工具参数或系统 PATH。
        input["nodeExecutable"] = if use_node {
            json!(private_node)
        } else {
            Value::Null
        };
        input["excludedRoots"] = json!([ctx.data_home, ctx.extension_root]);
        // Trust only the exact link-free workspace for this conversation, never its surrounding formal data.
        // 仅信任当前聊天的精确无链接工作范围，不能放行其周围正式应用数据。
        let own_sandbox = ctx.data_home.join("Sandboxes").join(&ctx.conversation_id);
        let managed = uuid::Uuid::parse_str(&ctx.conversation_id).is_ok()
            && super::local_files::inspect(&ctx.workdir, false, true).is_ok()
            && super::service_filesystem::canonical(&own_sandbox, false)
                .ok()
                .zip(super::service_filesystem::canonical(&ctx.workdir, false).ok())
                .is_some_and(|(sandbox, work)| {
                    super::local_files::within(&sandbox, &work)
                        && super::local_files::within(&work, &sandbox)
                });
        input["trustedManagedWorkspace"] = json!(managed);
        input["conversationWorkspaceHome"] = if managed {
            json!(ctx.data_home)
        } else {
            Value::Null
        };
        let mut result = invoke(input, 130_000, ctx, None).await?;
        // Verify the helper's isolation and descendant cleanup before accepting a receipt.
        // 收据必须证明隔离与子进程清理；不能把成功读取 JSON 当成沙箱执行已核验。
        native(ctx, "runner.sandboxReceipt", json!({"result":result})).await?;
        let completion = native(
            ctx,
            "broker.completion.sandboxReceipt",
            json!({"name":"terminal.run","value":result}),
        )
        .await?;
        // Isolation proves where execution ran, not whether the command or skill succeeded.
        // 隔离证明仅说明执行位置；命令或技能是否成功还必须保留真实退出、超时和取消状态。
        for (field, field_value) in completion.as_object().unwrap() {
            result[field] = field_value.clone();
        }
        return Ok(result);
    }
    if let Some(action) = name.strip_prefix("computer.") {
        let target = json!({
            "appPath": input["appPath"],
            "windowId": input["windowId"],
            "processId": input["processId"]
        });
        let forbidden = super::intent::calculate(
            "tools.foregroundForbidden",
            &json!({"message":ctx.config["trustedUserText"],"target":target}),
        )
        .map_err(|message| failure("TOOL_INTENT_FAILED", &message, 500))?;
        if (action == "activate" || action == "launch" && input["background"] == false)
            && forbidden == true
        {
            return Err(failure(
                "DESKTOP_FOREGROUND_FORBIDDEN",
                "用户明确要求保持后台，未激活窗口。",
                403,
            ));
        }
        if action == "launch" {
            let basename = Path::new(input["appPath"].as_str().unwrap_or(""))
                .file_name()
                .and_then(|name| name.to_str())
                .unwrap_or("")
                .to_lowercase();
            let intent = super::intent::calculate(
                "tools.browserIntent",
                &json!({
                    "message": ctx.config["trustedUserText"],
                    "previousUserMessages": ctx.config["previousUserMessages"]
                }),
            )
            .map_err(|message| failure("TOOL_INTENT_FAILED", &message, 500))?;
            if intent["excludedBrowserExecutables"]
                .as_array()
                .is_some_and(|excluded| {
                    excluded
                        .iter()
                        .any(|value| value.as_str() == Some(basename.as_str()))
                })
            {
                return Err(failure(
                    "BROWSER_APPLICATION_FORBIDDEN",
                    "用户禁止使用此浏览器，未启动应用。",
                    403,
                ));
            }
        }
        let keys = json!([
            "ENTER",
            "TAB",
            "ESC",
            "BACKSPACE",
            "DELETE",
            "UP",
            "DOWN",
            "LEFT",
            "RIGHT",
            "HOME",
            "END",
            "PAGEUP",
            "PAGEDOWN",
            "CTRL+A",
            "CTRL+C",
            "CTRL+V",
            "CTRL+S",
            "CTRL+F",
            "CTRL+L",
            "CTRL+Z",
            "CTRL+Y",
            "SHIFT+TAB",
            "CTRL+ENTER",
            "CTRL+PLUS",
            "CTRL+MINUS",
            "CTRL+0"
        ]);
        if action == "launch" && input["background"].is_null() {
            input["background"] = json!(true);
        }
        let path = Path::new(input["appPath"].as_str().unwrap_or(""));
        let plan = native(
            ctx,
            "runner.desktopRequest",
            json!({
                "action": action,
                "input": input,
                "keys": keys,
                "pathAbsolute": path.is_absolute(),
                "basename": path.file_name().and_then(|value| value.to_str()).unwrap_or("").to_lowercase()
            }),
        )
        .await?;
        let mut request = input.clone();
        request.as_object_mut().unwrap().remove("reason");
        request["operation"] = json!("desktop");
        request["action"] = json!(action);
        let value = invoke(
            request,
            plan["timeoutMs"].as_u64().unwrap_or(20_000),
            ctx,
            None,
        )
        .await?;
        if value["outcome"] == "unknown" {
            return Ok(value);
        }
        return desktop_receipt(action, &input, value, ctx).await;
    }
    Err(failure("TOOL_NOT_FOUND", "终端或桌面操作不存在。", 404))
}

/// Apply the receipt plan without substituting its control fields for ToolHost's actual result.
/// 校验计划仅控制投影，不能取代 ToolHost 的实际回执，否则窗口和进程身份会丢失。
async fn desktop_receipt(
    action: &str,
    parameters: &Value,
    mut value: Value,
    ctx: &GatewayToolContext,
) -> Result<Value, GatewayError> {
    if value["error"].is_object() && value["error"]["partial"] != true {
        let code = value["error"]["code"].as_str().unwrap_or("DESKTOP_FAILED");
        let message = value["error"]["message"]
            .as_str()
            .unwrap_or("原生桌面操作失败。");
        if action == "launch"
            && ![
                "DESKTOP_LAUNCH_BLOCKED",
                "DESKTOP_LAUNCH_FAILED",
                "DESKTOP_INVALID_REQUEST",
                "DESKTOP_UNAVAILABLE",
            ]
            .contains(&code)
        {
            // A failed observation after launch cannot certify that no application was started.
            // 启动后的观察失败不能证明未启动；保留未知状态，阻止盲目重复启动。
            return Ok(json!({
                "action": action,
                "boundary": "host-desktop",
                "completed": false,
                "outcome": "unknown",
                "status": "unknown",
                "isError": true,
                "code": code,
                "error": value["error"]
            }));
        }
        let status = match code {
            "DESKTOP_CANCELLED" => 499,
            "DESKTOP_TIMEOUT" | "DESKTOP_READ_TIMEOUT" => 504,
            "DESKTOP_UNAVAILABLE" => 503,
            "DESKTOP_INVALID_REQUEST" | "DESKTOP_INVALID_TARGET" => 400,
            "DESKTOP_TARGET_CHANGED" | "DESKTOP_BUSY" => 409,
            "DESKTOP_ACCESS_DENIED" | "DESKTOP_LAUNCH_BLOCKED" | "DESKTOP_ACTIVATION_BLOCKED" => {
                403
            }
            _ => 502,
        };
        return Err(failure(code, message, status));
    }
    let plan = native(
        ctx,
        "runner.desktopResult",
        json!({
            "action": action,
            "value": value,
            "parameters": parameters,
            "aborted": *ctx.cancellation.borrow()
        }),
    )
    .await?;
    match plan["kind"].as_str() {
        Some("value") => Ok(value),
        Some("partial" | "unconfirmed") => {
            let mut projected = plan["projectedValue"].clone();
            for field in ["isError", "status", "code"] {
                if let Some(field_value) = plan.get(field) {
                    projected[field] = field_value.clone();
                }
            }
            Ok(projected)
        }
        Some("screenshot") => {
            let encoded = value.as_object_mut().unwrap().remove("data").unwrap();
            value["imagePreview"] = plan["imagePreview"].clone();
            Ok(json!({
                "content": [{ "type": "image", "mimeType": "image/png", "data": encoded }],
                "structuredContent": value,
                "isError": false
            }))
        }
        _ => Err(failure(
            "DESKTOP_INVALID_RESULT",
            "原生桌面回执投影计划无效。",
            502,
        )),
    }
}

fn unknown_job(id: &str, message: &str) -> Value {
    json!({
        "jobId": id,
        "status": "unknown",
        "isError": true,
        "code": "HOST_TERMINAL_EXECUTION_UNKNOWN",
        "message": message,
        "recoveryHandle": { "kind": "host-terminal-job", "jobId": id }
    })
}

/// Cancel owned background work before cleaning verified sandbox stages; launched GUI apps stay independent.
/// 先取消自有后台任务，再清理已核验的沙箱阶段；已启动的独立 GUI 应用保持其自身生命周期。
pub async fn shutdown(ctx: &GatewayToolContext) {
    let jobs = ctx
        .runtime
        .jobs
        .lock()
        .await
        .values()
        .cloned()
        .collect::<Vec<_>>();
    for job in &jobs {
        let _ = job.cancel.send(true);
    }
    for job in jobs {
        let mut finished = job.finished;
        if !*finished.borrow() {
            let _ = tokio::time::timeout(Duration::from_secs(8), finished.changed()).await;
        }
    }
    let (_sender, cancellation) = watch::channel(false);
    let cleanup_context = GatewayToolContext {
        cancellation,
        ..ctx.clone()
    };
    if let Ok(stages) = native(&cleanup_context, "runner.sandboxStages", json!({})).await {
        for stage in stages.as_array().into_iter().flatten() {
            if let Ok(plan) = native(
                &cleanup_context,
                "runner.sandboxCleanupPlan",
                json!({"stage":stage}),
            )
            .await
            {
                if native(&cleanup_context, "runner.sandboxCleanup", plan)
                    .await
                    .is_ok()
                {
                    let _ = native(
                        &cleanup_context,
                        "runner.sandboxCleaned",
                        json!({"stage":stage}),
                    )
                    .await;
                }
            }
        }
    }
}

pub(crate) async fn capabilities(kind: &str, ctx: &GatewayToolContext) -> Value {
    let operation = match kind {
        "host" => "host_terminal_capabilities",
        "desktop" => "desktop_capabilities",
        _ => "capabilities",
    };
    match invoke(json!({"operation":operation}), 10000, ctx, None).await {
        Ok(value) => value,
        Err(error) => native(
            ctx,
            "runner.capabilitiesFailure",
            json!({"kind":kind,"message":error.message}),
        )
        .await
        .unwrap_or(json!({"available":false,"reason":error.message})),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn receipt_desktop_discovery_and_launch_preserve_native_identity() {
        let (ctx, _sender) = super::super::gateway::test_context();
        let root = ctx.workdir.parent().unwrap().to_owned();
        for (action, parameters, fields) in [
            (
                "apps",
                json!({}),
                json!({"apps":[{"path":"C:/Fixture/app.exe"}],"source":"fixture"}),
            ),
            (
                "windows",
                json!({"processId":42}),
                json!({"windows":[{"processId":42,"windowId":"123","title":"Fixture"}],"truncated":false}),
            ),
            (
                "launch",
                json!({"background":true}),
                json!({"processId":42,"appPath":"C:/Fixture/app.exe","backgroundRequested":true,
                "backgroundMode":"best-effort-no-activate","windowDiscoveryRequired":true}),
            ),
        ] {
            let mut original = fields;
            original["protocolVersion"] = json!(1);
            original["boundary"] = json!("host-desktop");
            original["action"] = json!(action);
            original["completed"] = json!(true);
            let result = desktop_receipt(action, &parameters, original.clone(), &ctx)
                .await
                .unwrap();
            assert_eq!(result, original);
            assert!(result.get("kind").is_none());
        }
        std::fs::remove_dir_all(root).unwrap();
    }

    #[tokio::test]
    async fn receipt_desktop_errors_and_unknown_effects_keep_actual_classification() {
        let (ctx, _sender) = super::super::gateway::test_context();
        let root = ctx.workdir.parent().unwrap().to_owned();
        let error = desktop_receipt(
            "windows",
            &json!({}),
            json!({"error":{"code":"DESKTOP_TARGET_CHANGED","message":"fixture changed"}}),
            &ctx,
        )
        .await
        .unwrap_err();
        assert_eq!(error.code, "DESKTOP_TARGET_CHANGED");
        assert_eq!(error.message, "fixture changed");
        assert_eq!(error.status, 409);
        let unconfirmed = desktop_receipt("launch", &json!({"background":true}),
            json!({"protocolVersion":1,"boundary":"host-desktop","action":"launch","completed":true}), &ctx).await.unwrap();
        assert_eq!(unconfirmed["status"], "unknown");
        assert_eq!(unconfirmed["isError"], true);
        assert_eq!(unconfirmed["nativeReportedCompleted"], true);
        let partial = desktop_receipt("type", &json!({}),
            json!({"error":{"code":"DESKTOP_FAILED","message":"delivered one event","partial":true,"deliveredInputEvents":1}}), &ctx).await.unwrap();
        assert_eq!(partial["status"], "unknown");
        assert_eq!(partial["deliveredInputEvents"], 1);
        let cancelled = desktop_receipt(
            "launch",
            &json!({}),
            json!({"error":{"code":"DESKTOP_CANCELLED","message":"cancelled after launch"}}),
            &ctx,
        )
        .await
        .unwrap();
        assert_eq!(cancelled["outcome"], "unknown");
        assert_eq!(cancelled["code"], "DESKTOP_CANCELLED");
        std::fs::remove_dir_all(root).unwrap();
    }

    #[tokio::test]
    async fn receipt_screenshot_preserves_local_image_and_native_metadata() {
        let (ctx, _sender) = super::super::gateway::test_context();
        let root = ctx.workdir.parent().unwrap().to_owned();
        let png = "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mP8/x8AAwMCAO+a9ZkAAAAASUVORK5CYII=";
        let receipt = desktop_receipt(
            "screenshot",
            &json!({}),
            json!({"protocolVersion":1,"boundary":"host-desktop",
            "action":"screenshot","completed":true,"windowId":"123","processId":42,
            "mimeType":"image/png","width":1,"height":1,"data":png}),
            &ctx,
        )
        .await
        .unwrap();
        assert_eq!(receipt["content"][0]["data"], png);
        assert_eq!(receipt["structuredContent"]["processId"], 42);
        assert!(receipt["structuredContent"].get("data").is_none());
        let invalid = desktop_receipt("screenshot", &json!({}), json!({"protocolVersion":1,"boundary":"host-desktop",
            "action":"screenshot","completed":true,"mimeType":"image/png","width":1,"height":1,"data":"invalid"}), &ctx).await.unwrap_err();
        assert_eq!(invalid.code, "DESKTOP_INVALID_RESULT");
        std::fs::remove_dir_all(root).unwrap();
    }

    #[tokio::test]
    async fn receipt_host_output_exit_status_and_background_completion_keep_full_facts() {
        let (ctx, _sender) = super::super::gateway::test_context();
        let root = ctx.workdir.parent().unwrap().to_owned();
        let parameters = json!({"shell":"cmd","cwd":ctx.workdir,"visible":false});
        for exit_code in [0, 1] {
            let original = json!({"protocolVersion":1,"boundary":"host-terminal","completed":true,
                "shell":"cmd","cwd":ctx.workdir,"processId":42,"exitCode":exit_code,
                "stdout":"fixture output","stderr":if exit_code==0{""}else{"fixture assertion failed"},"activeProcessesAfterExit":0});
            let receipt = host_receipt(&parameters, &json!(0), original.clone(), &ctx)
                .await
                .unwrap();
            assert_eq!(receipt["value"], original);
            assert_eq!(receipt["isError"], exit_code != 0, "{receipt}");
            let plan = native(&ctx, "host.start", json!({"context":{"conversationId":ctx.conversation_id},
                "request":{"shell":"cmd","cwd":ctx.workdir,"command":"fixture"},"startedAt":"2026-10-12T00:00:00Z"})).await.unwrap();
            let id = plan["jobId"].as_str().unwrap();
            native(&ctx, "host.started", json!({"jobId":id,"processId":42}))
                .await
                .unwrap();
            native(&ctx, "host.complete", json!({"jobId":id,"result":receipt}))
                .await
                .unwrap();
            let view = native(
                &ctx,
                "host.view",
                json!({"context":{"conversationId":ctx.conversation_id},"jobId":id}),
            )
            .await
            .unwrap();
            assert_eq!(view["receipt"]["exitCode"], exit_code);
            assert_eq!(view["receipt"]["cwd"], json!(ctx.workdir));
            assert_eq!(
                view["status"],
                if exit_code == 0 { "completed" } else { "error" }
            );
        }
        let malformed = json!({"protocolVersion":1,"boundary":"host-terminal","completed":true,
            "shell":"powershell","cwd":ctx.workdir,"exitCode":0,"stdout":"retained partial evidence","stderr":"","activeProcessesAfterExit":0});
        let unknown = host_receipt(&parameters, &json!(0), malformed, &ctx)
            .await
            .unwrap();
        assert_eq!(unknown["status"], "unknown");
        assert_eq!(unknown["value"]["completed"], false);
        assert_eq!(unknown["value"]["nativeReportedCompleted"], true);
        assert_eq!(unknown["value"]["stdout"], "retained partial evidence");
        let timed_out = host_receipt(&parameters, &json!(0), json!({"protocolVersion":1,"boundary":"host-terminal",
            "completed":false,"outcome":"unknown","timedOut":true,"stdout":"partial before timeout"}), &ctx).await.unwrap();
        assert_eq!(timed_out["status"], "unknown");
        assert_eq!(timed_out["code"], "TOOL_TIMED_OUT");
        assert_eq!(timed_out["value"]["stdout"], "partial before timeout");
        std::fs::remove_dir_all(root).unwrap();
    }

    #[cfg(windows)]
    #[tokio::test]
    async fn receipt_real_toolhost_discovery_and_terminal_output_cross_the_gateway_adapter() {
        let Some(executable) = std::env::var_os("KYNXA_TEST_TOOLHOST_EXE") else {
            return;
        };
        let (mut ctx, _sender) = super::super::gateway::test_context();
        let root = ctx.workdir.parent().unwrap().to_owned();
        ctx.toolhost_exe = executable.into();
        ctx.approved = true;
        for (name, array) in [("computer.apps", "apps"), ("computer.windows", "windows")] {
            let input = if array == "windows" {
                json!({"processId":std::process::id()})
            } else {
                json!({})
            };
            let receipt = execute(name, input, &ctx).await.unwrap();
            assert!(receipt[array].is_array());
            assert_eq!(receipt["completed"], true);
            assert_eq!(receipt["boundary"], "host-desktop");
            assert!(receipt.get("kind").is_none());
        }
        for exit_code in [0, 1] {
            let receipt = execute(
                "terminal.host.run",
                json!({
                    "shell": "cmd",
                    "script": format!("echo fixture-stdout & echo fixture-stderr 1>&2 & exit /b {exit_code}"),
                    "cwd": ctx.workdir,
                    "timeoutMs": 5000,
                    "reason": "isolated gateway receipt fixture"
                }),
                &ctx,
            )
            .await
            .unwrap();
            assert_eq!(receipt["value"]["exitCode"], exit_code);
            assert!(
                receipt["value"]["stdout"]
                    .as_str()
                    .unwrap()
                    .contains("fixture-stdout")
            );
            assert!(
                receipt["value"]["stderr"]
                    .as_str()
                    .unwrap()
                    .contains("fixture-stderr")
            );
            assert_eq!(receipt["isError"], exit_code != 0, "{receipt}");
        }
        let sandbox = execute("terminal.run", json!({"command":"cmd",
            "args":["/d","/c","echo sandbox-stdout & echo sandbox-stderr 1>&2 & exit /b 1"],"timeoutMs":10000}), &ctx).await.unwrap();
        assert_eq!(sandbox["exitCode"], 1);
        assert_eq!(sandbox["isError"], true);
        assert!(
            sandbox["stdout"]
                .as_str()
                .unwrap()
                .contains("sandbox-stdout")
        );
        assert!(
            sandbox["stderr"]
                .as_str()
                .unwrap()
                .contains("sandbox-stderr")
        );
        std::fs::remove_dir_all(root).unwrap();
    }

    #[cfg(windows)]
    #[tokio::test]
    async fn owned_job_memory_samples_only_live_owned_handles_and_confirms_release() {
        let root = std::env::var_os("SystemRoot").unwrap();
        let executable = Path::new(&root).join("System32/WindowsPowerShell/v1.0/powershell.exe");
        let mut command = Command::new(executable);
        hide(&mut command);
        command
            .args([
                "-NoProfile",
                "-NonInteractive",
                "-Command",
                "[Console]::WriteLine('ready'); Start-Sleep -Seconds 30",
            ])
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .kill_on_drop(true);
        let mut child = command.spawn().unwrap();
        let owned = OwnedProcessJob::attach(&child).unwrap();
        let mut output = BufReader::new(child.stdout.take().unwrap());
        assert_eq!(
            tokio::time::timeout(Duration::from_secs(5), bounded_line(&mut output, 100))
                .await
                .unwrap()
                .unwrap()
                .as_deref(),
            Some("ready")
        );
        let sample = owned.memory_usage().unwrap();
        // Windows may create an owned console helper; the verified job tree is the measurement boundary.
        // Windows 可能创建自有控制台辅助进程；采样边界是已核验的作业树，不固定为一个进程。
        assert!(sample.processes >= 1);
        assert!(sample.private_bytes > 0 && sample.working_set_bytes > 0);
        owned.terminate_and_wait().await.unwrap();
        child.wait().await.unwrap();
        assert_eq!(owned.memory_usage().unwrap().processes, 0);
        assert!(!owned.has_running_processes().unwrap());
    }
    #[tokio::test]
    async fn native_frames_are_bounded_before_newline_and_preserve_split_utf8() {
        let (mut writer, reader) = tokio::io::duplex(16);
        let mut reader = BufReader::new(reader);
        let sent = tokio::spawn(async move {
            writer.write_all("中".as_bytes()).await.unwrap();
            writer.write_all(b"\r\nnext\n").await.unwrap();
        });
        assert_eq!(
            bounded_line(&mut reader, 16).await.unwrap(),
            Some("中".into())
        );
        assert_eq!(
            bounded_line(&mut reader, 16).await.unwrap(),
            Some("next".into())
        );
        sent.await.unwrap();
        let (mut writer, reader) = tokio::io::duplex(16);
        writer.write_all(b"123456789").await.unwrap();
        assert!(bounded_line(&mut BufReader::new(reader), 8).await.is_err());
    }
    #[tokio::test]
    async fn unknown_job_proof_reads_owned_registry_and_never_certifies_task_success() {
        let (ctx, _sender) = super::super::gateway::test_context();
        let root = ctx.workdir.parent().unwrap().to_owned();
        let plan = native(
            &ctx,
            "host.start",
            json!({
                "context": { "conversationId": ctx.conversation_id },
                "request": { "shell": "cmd", "cwd": ctx.workdir, "command": "echo ok" },
                "startedAt": "2026-10-12T00:00:00Z"
            }),
        )
        .await
        .unwrap();
        let id = plan["jobId"].as_str().unwrap();
        let (cancel, _) = watch::channel(false);
        let (_finished, finished) = watch::channel(true);
        ctx.runtime.jobs.lock().await.insert(
            id.into(),
            HostJob {
                conversation_id: ctx.conversation_id.clone(),
                cancel,
                result: Arc::new(Mutex::new(None)),
                finished,
            },
        );
        let records = vec![
            json!({"call":{"id":"start-call","name":"terminal.host.start"},"result":unknown_job(id,"unknown")}),
        ];
        let observation = json!({"call":{"id":"read-call","name":"terminal.host.read","arguments":{"jobId":id}},"result":{"isError":false}});
        assert!(
            super::super::gateway::verify_unknown_effects(&ctx, &records, &observation)
                .await
                .unwrap()
                .is_empty()
        );
        native(&ctx, "host.started", json!({"jobId":id,"processId":42}))
            .await
            .unwrap();
        let proofs = super::super::gateway::verify_unknown_effects(&ctx, &records, &observation)
            .await
            .unwrap();
        assert_eq!(proofs[0]["result"]["value"]["dispatchConfirmed"], true);
        assert_eq!(proofs[0]["result"]["value"]["taskSuccessCertified"], false);
        let mut foreign = ctx.clone();
        foreign.conversation_id = uuid::Uuid::new_v4().to_string();
        assert!(
            super::super::gateway::verify_unknown_effects(&foreign, &records, &observation)
                .await
                .unwrap()
                .is_empty()
        );
        std::fs::remove_dir_all(root).unwrap();
    }
}

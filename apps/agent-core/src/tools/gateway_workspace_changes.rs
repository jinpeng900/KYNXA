//! A coarse workspace diff operation reads only the current authorized directory and never invokes external diff helpers.
//! 工作区变更采用一次粗粒度读取，只覆盖当前授权目录，不执行外部 diff 辅助程序。

use super::gateway::{GatewayToolContext, check_cancelled, failure};
use crate::gateway_contract::GatewayError;
use serde_json::{Value, json};
use std::{process::Stdio, time::Duration};
use tokio::io::AsyncReadExt;

async fn git(
    arguments: &[&str],
    context: &GatewayToolContext,
) -> Result<(String, bool), GatewayError> {
    check_cancelled(context)?;
    let mut command = tokio::process::Command::new("git");
    command
        .arg("-c")
        .arg("core.quotepath=false")
        .arg("-C")
        .arg(&context.workdir)
        .args(arguments)
        .env("GIT_TERMINAL_PROMPT", "0")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    super::gateway_process::hide(&mut command);
    let mut child = command.spawn().map_err(|_| {
        failure(
            "WORKSPACE_GIT_UNAVAILABLE",
            "本机未找到 Git，仍可继续查看文件与聊天。",
            503,
        )
    })?;
    let owner = super::gateway_process::OwnedProcessJob::attach(&child)?;
    let stdout = child.stdout.take().unwrap();
    let stderr = child.stderr.take().unwrap();
    let mut cancellation = context.cancellation.clone();
    let operation = async {
        let output = tokio::spawn(async move {
            let mut bytes = Vec::new();
            stdout
                .take(5 * 1024 * 1024 + 1)
                .read_to_end(&mut bytes)
                .await
                .map(|_| bytes)
        });
        let errors = tokio::spawn(async move {
            let mut bytes = Vec::new();
            stderr
                .take(64 * 1024)
                .read_to_end(&mut bytes)
                .await
                .map(|_| bytes)
        });
        let output = output
            .await
            .map_err(|_| failure("WORKSPACE_GIT_FAILED", "变更读取线程结束。", 500))??;
        let truncated = output.len() > 5 * 1024 * 1024;
        if truncated {
            super::gateway_process::terminate(&mut child).await;
        }
        let errors = errors
            .await
            .map_err(|_| failure("WORKSPACE_GIT_FAILED", "变更错误读取线程结束。", 500))??;
        let status = child.wait().await?;
        if !status.success() && !truncated {
            let message = String::from_utf8_lossy(&errors);
            return Err(failure(
                "WORKSPACE_GIT_FAILED",
                if message.contains("not a git repository") {
                    "当前工作目录不是 Git 仓库。"
                } else {
                    "Git 未能读取当前工作目录的变更。"
                },
                422,
            ));
        }
        let output = &output[..output.len().min(5 * 1024 * 1024)];
        Ok((String::from_utf8_lossy(output).into_owned(), truncated))
    };
    let result = tokio::select! {
        result = operation => result,
        _ = super::gateway::wait_for_cancellation(&mut cancellation) => Err(failure("TOOL_CANCELLED", "变更查看已取消。", 409)),
        _ = tokio::time::sleep(Duration::from_secs(30)) => Err(failure("WORKSPACE_GIT_TIMEOUT", "读取工作区变更超时，可继续使用文件和聊天。", 504)),
    };
    drop(owner);
    result
}

pub(crate) async fn changes(context: &GatewayToolContext) -> Result<Value, GatewayError> {
    let (status, unstaged, staged) = tokio::try_join!(
        git(
            &["status", "--short", "--untracked-files=normal", "--", "."],
            context
        ),
        git(
            &[
                "diff",
                "--no-ext-diff",
                "--no-textconv",
                "--no-color",
                "--",
                "."
            ],
            context
        ),
        git(
            &[
                "diff",
                "--cached",
                "--no-ext-diff",
                "--no-textconv",
                "--no-color",
                "--",
                "."
            ],
            context
        ),
    )?;
    let text = format!(
        "# 工作区状态 / Workspace status\n{}\n# 未暂存 / Unstaged\n{}\n# 已暂存 / Staged\n{}",
        if status.0.is_empty() {
            "工作区没有未提交变更。"
        } else {
            &status.0
        },
        unstaged.0,
        staged.0
    );
    let version = super::local_files::sha256(text.as_bytes());
    Ok(
        json!({"kind":"code","title":"未提交变更","path":"changes.diff","language":"diff","text":text,
        "version":version,"mimeType":"text/x-diff","truncated":status.1||unstaged.1||staged.1,"workdir":context.workdir}),
    )
}

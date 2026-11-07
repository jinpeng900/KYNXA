//! 后台安装限制输出并拥有进程树，取消不留下安装子进程。
use super::{Setup, cancelled, update};
use crate::{gateway_contract::GatewayError, tools::gateway_process::OwnedProcessJob};
use std::{path::Path, process::Stdio, time::Duration};
use tokio::{
    io::{AsyncRead, AsyncReadExt},
    process::Command,
    sync::{mpsc, watch},
};

pub(super) fn command(program: &Path, work: &Path) -> Command {
    let mut command = Command::new(program);
    command
        .current_dir(work)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    for (key, _) in std::env::vars_os() {
        if key.to_string_lossy().starts_with("UV_") || key.to_string_lossy().starts_with("PIP_") {
            command.env_remove(key);
        }
    }
    command
        .env("PYTHONIOENCODING", "utf-8")
        .env("PYTHONNOUSERSITE", "1")
        .env("OMP_NUM_THREADS", "2")
        .env("MKL_NUM_THREADS", "2");
    #[cfg(windows)]
    command.creation_flags(0x0800_0000 | 0x0000_4000);
    command
}

async fn pump(mut source: impl AsyncRead + Unpin + Send + 'static, tx: mpsc::Sender<Vec<u8>>) {
    let mut buffer = [0u8; 2048];
    while let Ok(length) = source.read(&mut buffer).await {
        if length == 0 || tx.send(buffer[..length].to_vec()).await.is_err() {
            break;
        }
    }
}

pub(super) async fn run(
    setup: &Setup,
    mut command: Command,
    mut stop: watch::Receiver<bool>,
    limit: Duration,
    log: bool,
) -> Result<String, GatewayError> {
    cancelled(&stop)?;
    let mut child = command.spawn()?;
    let tree = OwnedProcessJob::attach(&child)?;
    if let (Some(service), Some(lease)) =
        (crate::resources::gateway::global(), setup.lease.as_ref())
    {
        service
            .request(
                "registerExecutor",
                serde_json::json!({"leaseId":lease,"processId":child.id()}),
                Duration::from_secs(5),
            )
            .await?;
    }
    let (tx, mut rx) = mpsc::channel(8);
    let stdout = tokio::spawn(pump(child.stdout.take().unwrap(), tx.clone()));
    let stderr = tokio::spawn(pump(child.stderr.take().unwrap(), tx));
    let mut captured = String::new();
    let deadline = tokio::time::sleep(limit);
    tokio::pin!(deadline);
    let result = loop {
        tokio::select! {
            _ = stop.changed() => { break Err(GatewayError::new("TRAINING_ENV_CANCELLED", "环境准备已取消。", 499)); }
            _ = &mut deadline => { break Err(GatewayError::new("TRAINING_ENV_TIMEOUT", "当前安装步骤等待过久，可稍后重新准备。原环境不受影响。", 504)); }
            block = rx.recv(), if !rx.is_closed() => {
                if let Some(block) = block {
                    let text = String::from_utf8_lossy(&block);
                    captured.push_str(&text);
                    if captured.len() > 32768 { let mut start = captured.len() - 16384; while !captured.is_char_boundary(start) { start += 1; } captured.drain(..start); }
                    if log { update(setup, None, Some(text.trim()))?; }
                }
            }
            status = child.wait() => {
                break status.map_err(GatewayError::from).and_then(|status| if status.success() { Ok(()) } else {
                    Err(GatewayError::new("TRAINING_ENV_STEP_FAILED", "环境准备步骤失败，请查看安装记录后重试。原环境未变更。", 400))
                });
            }
        }
    };
    if result.is_err() {
        let _ = child.start_kill();
    }
    drop(tree);
    let _ = child.wait().await;
    // 退出后收齐尾部输出；限量通道不会随安装日志增长。
    while let Some(block) = rx.recv().await {
        let text = String::from_utf8_lossy(&block);
        captured.push_str(&text);
        if captured.len() > 32768 {
            let mut start = captured.len() - 16384;
            while !captured.is_char_boundary(start) {
                start += 1;
            }
            captured.drain(..start);
        }
        if log {
            let _ = update(setup, None, Some(text.trim()));
        }
    }
    let _ = stdout.await;
    let _ = stderr.await;
    result?;
    Ok(captured)
}

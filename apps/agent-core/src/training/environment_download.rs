//! 固定上游运行包先验哈希再解包执行，不运行网络安装脚本。
use super::{Setup, cancelled, update};
use crate::{gateway_contract::GatewayError, storage::managed_files};
use futures_util::StreamExt;
use sha2::{Digest, Sha256};
use std::{
    io::{Cursor, Read},
    path::{Path, PathBuf},
    time::Duration,
};
use tokio::sync::watch;

const UV_URL: &str =
    "https://github.com/astral-sh/uv/releases/download/0.9.7/uv-x86_64-pc-windows-msvc.zip";
const UV_HASH: &str = "5d250c32d3604e28dbe18dc65c668ff628c53e00dde2c642576e831e4a60da64";
const PYTHON_URL: &str = "https://github.com/astral-sh/python-build-standalone/releases/download/20251028/cpython-3.12.12%2B20251028-x86_64-pc-windows-msvc-install_only_stripped.tar.gz";
const PYTHON_HASH: &str = "b01ce5aed9bae58023ccfba76c9cf0e789aa2ca36057ee6e6af054da2a2d4eb1";
fn network(error: reqwest::Error) -> GatewayError {
    let message = if error.is_timeout() {
        "下载等待超时，请检查网络或代理后重试。"
    } else {
        "运行包下载失败，请检查网络或代理后重试。"
    };
    GatewayError::new("TRAINING_ENV_NETWORK", message, 503)
}

async fn fetch(
    setup: &Setup,
    url: &str,
    expected: &str,
    mut stop: watch::Receiver<bool>,
) -> Result<Vec<u8>, GatewayError> {
    let builder = crate::models::gateway::training_download_client_builder(url)
        .await
        .map_err(|e| GatewayError::new("TRAINING_ENV_NETWORK", &e.message, 503))?;
    let client = builder
        .connect_timeout(Duration::from_secs(20))
        .read_timeout(Duration::from_secs(60))
        .build()
        .map_err(network)?;
    let response = tokio::select! {
        result = client.get(url).send() => result.map_err(network)?,
        _ = stop.changed() => { cancelled(&stop)?; return Err(super::cancel_error()); }
    }
    .error_for_status()
    .map_err(network)?;
    let total = response.content_length();
    let mut body = response.bytes_stream();
    let mut bytes = Vec::new();
    let mut digest = Sha256::new();
    let mut last = std::time::Instant::now();
    while let Some(block) = tokio::select! { value = body.next() => value, _ = stop.changed() => { return Err(super::cancel_error()); } }
    {
        let block = block.map_err(network)?;
        if bytes.len().saturating_add(block.len()) > 96 * 1024 * 1024 {
            return Err(super::invalid("运行包超过大小限制。"));
        }
        digest.update(&block);
        bytes.extend_from_slice(&block);
        if last.elapsed() >= Duration::from_millis(500) {
            super::patch(
                setup,
                serde_json::json!({"downloadedBytes":bytes.len(),"totalBytes":total}),
            )?;
            last = std::time::Instant::now();
        }
    }
    if format!("{:x}", digest.finalize()) != expected {
        return Err(GatewayError::new(
            "TRAINING_ENV_HASH_MISMATCH",
            "运行包完整性校验失败，未执行下载内容。",
            409,
        ));
    }
    super::patch(
        setup,
        serde_json::json!({"downloadedBytes":bytes.len(),"totalBytes":bytes.len()}),
    )?;
    Ok(bytes)
}

fn cached(setup: &Setup, dir: &Path, executable: &Path, source_hash: &str) -> bool {
    let manifest = managed_files::read(&setup.context.data_home, &dir.join("verified.json"), 4096)
        .ok()
        .flatten()
        .and_then(|v| serde_json::from_slice::<serde_json::Value>(&v).ok());
    let bytes = managed_files::read(&setup.context.data_home, executable, 128 * 1024 * 1024)
        .ok()
        .flatten();
    match (manifest, bytes) {
        (Some(meta), Some(bytes)) => {
            meta["sourceHash"] == source_hash
                && meta["executableHash"] == managed_files::digest(&bytes)
        }
        _ => false,
    }
}

pub(super) async fn runtime(
    setup: &Setup,
    python: bool,
    stop: watch::Receiver<bool>,
) -> Result<PathBuf, GatewayError> {
    cancelled(&stop)?;
    if !cfg!(all(windows, target_arch = "x86_64")) {
        return Err(super::invalid(
            "自动下载运行环境目前支持 Windows x64；可手动选择本机 Python。",
        ));
    }
    let dir = setup
        .context
        .data_home
        .join("Runtime/training-bootstrap")
        .join(if python {
            "python-3.12.12-20251028"
        } else {
            "uv-0.9.7"
        });
    let executable = dir.join(if python {
        "python/python.exe"
    } else {
        "uv.exe"
    });
    if cached(
        setup,
        &dir,
        &executable,
        if python { PYTHON_HASH } else { UV_HASH },
    ) {
        return Ok(executable);
    }
    update(
        setup,
        Some(if python {
            "downloading-python"
        } else {
            "downloading-installer"
        }),
        Some(if python {
            "下载 Python 3.12 运行环境（Astral 官方发布）。"
        } else {
            "下载独立依赖安装器（Astral 官方发布）。"
        }),
    )?;
    let bytes = fetch(
        setup,
        if python { PYTHON_URL } else { UV_URL },
        if python { PYTHON_HASH } else { UV_HASH },
        stop.clone(),
    )
    .await?;
    cancelled(&stop)?;
    let setup = setup.clone();
    tokio::task::spawn_blocking(move || extract(&setup, python, &dir, &executable, bytes, stop))
        .await
        .map_err(|_| super::invalid("运行包解压未完成。原环境保留。"))?
}

fn extract(
    setup: &Setup,
    python: bool,
    dir: &Path,
    executable: &Path,
    bytes: Vec<u8>,
    stop: watch::Receiver<bool>,
) -> Result<PathBuf, GatewayError> {
    managed_files::inspect(&setup.context.data_home, &dir, true, false, u64::MAX)?;
    if python {
        let archive = flate2::read::GzDecoder::new(Cursor::new(bytes));
        let mut archive = tar::Archive::new(archive);
        let mut extracted = 0u64;
        for entry in archive.entries()? {
            cancelled(&stop)?;
            let mut entry = entry?;
            let relative = entry.path()?.into_owned();
            if !relative.starts_with("python")
                || relative
                    .components()
                    .any(|c| !matches!(c, std::path::Component::Normal(_)))
            {
                return Err(super::invalid("运行包包含无效路径。"));
            }
            let target = dir.join(relative);
            if entry.header().entry_type().is_dir() {
                managed_files::inspect(&setup.context.data_home, &target, true, false, u64::MAX)?;
            } else if entry.header().entry_type().is_file() {
                extracted = extracted.saturating_add(entry.size());
                if entry.size() > 64 * 1024 * 1024 || extracted > 512 * 1024 * 1024 {
                    return Err(super::invalid("运行包解压大小异常。"));
                }
                let mut content = Vec::new();
                entry.read_to_end(&mut content)?;
                managed_files::atomic_write(
                    &setup.context.data_home,
                    &target,
                    &content,
                    64 * 1024 * 1024,
                )?;
            } else {
                return Err(super::invalid("运行包包含不支持的链接。"));
            }
        }
    } else {
        let mut archive = zip::ZipArchive::new(Cursor::new(bytes))
            .map_err(|_| super::invalid("安装器压缩包无法读取。"))?;
        let mut entry = archive
            .by_name("uv.exe")
            .map_err(|_| super::invalid("安装器压缩包缺少执行文件。"))?;
        if entry.size() > 128 * 1024 * 1024 {
            return Err(super::invalid("安装器大小异常。"));
        }
        let mut content = Vec::new();
        entry.read_to_end(&mut content)?;
        managed_files::atomic_write(
            &setup.context.data_home,
            &executable,
            &content,
            128 * 1024 * 1024,
        )?;
    }
    let content = managed_files::read(&setup.context.data_home, &executable, 128 * 1024 * 1024)?
        .ok_or_else(|| super::invalid("运行包缺少执行文件。"))?;
    super::store::write(
        &setup.context,
        &dir.join("verified.json"),
        &serde_json::json!({"sourceHash":if python{PYTHON_HASH}else{UV_HASH},"executableHash":managed_files::digest(&content)}),
    )?;
    Ok(executable.to_path_buf())
}

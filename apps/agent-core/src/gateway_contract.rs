//! Shared native gateway contracts; transport never owns a second business ledger.
//! 原生网关共享合同；传输层不维护第二份业务账本。

use serde_json::{Value, json};
use std::path::PathBuf;

#[derive(Debug, Clone)]
pub struct GatewayError {
    pub code: String,
    pub message: String,
    pub status: u16,
    pub retry_after_ms: Option<u64>,
}

impl GatewayError {
    pub fn new(code: impl Into<String>, message: impl Into<String>, status: u16) -> Self {
        Self {
            code: code.into(),
            message: message.into(),
            status,
            retry_after_ms: None,
        }
    }

    pub fn response(&self) -> Value {
        let mut response = json!({"error": self.message, "code": self.code});
        if let Some(milliseconds) = self.retry_after_ms {
            response["retryAfterMs"] = json!(milliseconds);
        }
        response
    }

    pub fn with_retry_after(mut self, milliseconds: Option<u64>) -> Self {
        self.retry_after_ms = milliseconds;
        self
    }
}

impl std::fmt::Display for GatewayError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "{}: {}", self.code, self.message)
    }
}
impl std::error::Error for GatewayError {}
impl From<std::io::Error> for GatewayError {
    fn from(_error: std::io::Error) -> Self {
        // Do not leak private host paths or credentials through public transport errors.
        // 公开传输错误不泄露宿主私有路径或凭据；具体操作另保留机器错误码。
        Self::new(
            "GATEWAY_IO_FAILED",
            "本机读写失败，请检查路径与运行权限。",
            500,
        )
    }
}
impl From<crate::storage::ContractError> for GatewayError {
    fn from(error: crate::storage::ContractError) -> Self {
        Self::new(error.code(), error.message(), error.status_code())
    }
}
impl From<crate::models::gateway::ModelError> for GatewayError {
    fn from(error: crate::models::gateway::ModelError) -> Self {
        Self::new(error.code, error.message, error.status_code)
    }
}

#[derive(Clone, Debug)]
pub struct GatewayConfig {
    pub data_home: PathBuf,
    pub data_root: PathBuf,
    pub extension_root: PathBuf,
    pub application_root: PathBuf,
    pub port: u16,
    pub toolhost_exe: PathBuf,
    pub vector_extension: PathBuf,
    pub inference_library: PathBuf,
}

impl GatewayConfig {
    pub fn from_environment() -> Result<Self, GatewayError> {
        let user_home = std::env::var_os("USERPROFILE")
            .or_else(|| std::env::var_os("HOME"))
            .map(PathBuf::from)
            .ok_or_else(|| {
                GatewayError::new("USER_HOME_UNAVAILABLE", "无法确定用户配置目录。", 500)
            })?;
        let arguments = json!({"userHome": user_home, "modelHome": std::env::var_os("KYNXA_MODEL_HOME").map(PathBuf::from),
            "dataHome": std::env::var_os("KYNXA_DATA_HOME").map(PathBuf::from)});
        let selected =
            crate::storage::administration::apply("storage.administration.modelHome", &arguments)
                .map_err(|_| {
                GatewayError::new(
                    "INVALID_STORAGE_CONFIGURATION",
                    "存储配置无效，原位置未改变。",
                    409,
                )
            })?;
        let data_home = PathBuf::from(selected.as_str().ok_or_else(|| {
            GatewayError::new("INVALID_STORAGE_CONFIGURATION", "模型目录无效。", 409)
        })?);
        let root = crate::storage::administration::apply(
            "storage.administration.conversationRoot",
            &json!({"dataHome": data_home}),
        )
        .map_err(|_| GatewayError::new("INVALID_STORAGE_CONFIGURATION", "数据目录无效。", 409))?;
        let data_root = PathBuf::from(root["path"].as_str().or_else(|| root.as_str()).ok_or_else(
            || GatewayError::new("INVALID_STORAGE_CONFIGURATION", "正式数据目录无效。", 409),
        )?);
        let application_root = match std::env::var_os("KYNXA_APPLICATION_ROOT") {
            Some(root) => PathBuf::from(root),
            None => {
                // Published layout is <application>/runtime/agent/kynxa-gateway.exe.
                // 发布布局以当前可执行文件定位，不把开发机源码路径作为用户运行路径。
                let executable = std::env::current_exe()?;
                executable
                    .parent()
                    .and_then(|directory| directory.parent())
                    .and_then(|directory| directory.parent())
                    .map(PathBuf::from)
                    .ok_or_else(|| {
                        GatewayError::new(
                            "APPLICATION_ROOT_UNAVAILABLE",
                            "无法确定应用运行目录。",
                            503,
                        )
                    })?
            }
        };
        let port = std::env::var("KYNXA_MODEL_API_PORT")
            .unwrap_or_else(|_| "5218".into())
            .parse()
            .map_err(|_| GatewayError::new("INVALID_GATEWAY_PORT", "模型网关端口无效。", 400))?;
        let runtime = application_root.join("runtime");
        let extension = crate::storage::extensions::apply("storage.extensions.home", &json!({
            "explicitRoot": std::env::var_os("KYNXA_EXTENSION_HOME").map(PathBuf::from),
            "isolated": std::env::var_os("KYNXA_EXTENSION_POINTER").is_none() &&
                (std::env::var_os("KYNXA_DATA_HOME").is_some() || std::env::var_os("KYNXA_MODEL_HOME").is_some()),
            "pointer": std::env::var_os("KYNXA_EXTENSION_POINTER").map(PathBuf::from)
                .unwrap_or_else(|| user_home.join(".kynxa").join("extensions.json")), "fallback": data_root
        })).map_err(|_| GatewayError::new("INVALID_EXTENSION_STORAGE", "扩展存储配置无效，原位置未改变。", 409))?;
        let extension_root = PathBuf::from(extension.as_str().ok_or_else(|| {
            GatewayError::new("INVALID_EXTENSION_STORAGE", "扩展目录无效。", 409)
        })?);
        Ok(Self {
            data_home,
            data_root,
            extension_root,
            toolhost_exe: runtime
                .parent()
                .unwrap()
                .join("ToolHost")
                .join("KYNXA.ToolHost.exe"),
            vector_extension: runtime.join("native").join("vec0.dll"),
            inference_library: runtime.join("native").join("onnxruntime.dll"),
            application_root,
            port,
        })
    }
}

pub fn now_iso8601() -> String {
    chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
}

pub fn new_id() -> String {
    uuid::Uuid::new_v4().to_string()
}

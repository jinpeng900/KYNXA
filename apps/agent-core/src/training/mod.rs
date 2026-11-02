//! 训练模块管理数据集、实验及检查点，聊天编排只调用正式工具。
mod dataset_preview;
pub(crate) mod datasets;
mod environment;
mod export;
pub(crate) mod imports;
mod jobs;
mod prediction;
mod records;
mod resources;
mod runtime;
pub(crate) mod store;
mod trial;

use crate::{
    gateway_contract::{GatewayConfig, GatewayError},
    tools::gateway::{self, GatewayToolContext},
};
use serde_json::{Value, json};
use std::sync::{Mutex, OnceLock};
static DATASETS: OnceLock<tokio::sync::Mutex<()>> = OnceLock::new();
static CONFIGURATION: Mutex<()> = Mutex::new(());
pub(super) fn invalid(message: &str) -> GatewayError {
    GatewayError::new("TRAINING_INVALID_CONFIGURATION", message, 400)
}

pub(crate) async fn handle(
    action: &str,
    input: &Value,
    context: &GatewayToolContext,
    config: &GatewayConfig,
) -> Result<Value, GatewayError> {
    gateway::check_cancelled(context)?;
    let action = action.strip_prefix("training.").unwrap_or(action);
    match action {
        "capabilities" => runtime::capabilities(context, config).await,
        "datasets.list" => Ok(json!({"datasets":store::list(context,"datasets","summary.json")?})),
        "sources.preview" => dataset_preview::source_preview(input, context).await,
        "datasets.read" => dataset_preview::read(input, context).await,
        "datasets.delete.preview" | "datasets.delete.apply" => {
            records::delete_dataset(action, input, context).await
        }
        "environment.read" | "environment.prepare" | "environment.cancel" => {
            environment::handle(action, input, context, config).await
        }
        "resources.preview" => resources::preview(input, context),
        "jobs.update" => records::update(input, context),
        "jobs.cleanup.preview" | "jobs.cleanup.apply" => {
            records::cleanup(action, input, context).await
        }
        "datasets.create" => {
            let _write = DATASETS.get_or_init(Default::default).lock().await;
            let id = uuid::Uuid::new_v4().to_string();
            let dir = store::scope_dir(context).join("datasets").join(&id);
            let mut dataset = datasets::create(input, context, config, &dir).await?;
            dataset["id"] = json!(id);
            dataset["createdAt"] = json!(chrono::Utc::now().to_rfc3339());
            dataset["version"] = json!(crate::storage::managed_files::digest(
                serde_json::to_vec(&dataset).unwrap().as_slice()
            ));
            store::write(context, &dir.join("dataset.json"), &dataset)?;
            let summary = store::summary(dataset);
            store::write(context, &dir.join("summary.json"), &summary)?;
            Ok(json!({"dataset":summary}))
        }
        "jobs.list" => jobs::list(context),
        "jobs.create" => jobs::create(input, context),
        "jobs.read" => jobs::read(input, context),
        "jobs.predict" => prediction::predict(input, context, config).await,
        "jobs.predict.schema" => prediction::schema(input, context, config).await,
        "jobs.trial.start" => trial::start(input, context, config).await,
        "jobs.trial.read" => trial::read(input, context),
        "jobs.trial.cancel" => trial::cancel(input, context),
        "jobs.start" | "jobs.resume" => jobs::start(input, context, config).await,
        "jobs.pause" | "jobs.stop" => jobs::control(action, input, context),
        "jobs.export" => export::export(input, context).await,
        _ => Err(invalid("此训练操作不可用。")),
    }
}
pub(crate) async fn tool(
    arguments: &Value,
    context: &GatewayToolContext,
) -> Result<Value, GatewayError> {
    let root = context.config["officialToolsRoot"]
        .as_str()
        .and_then(|p| std::path::Path::new(p).parent())
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| {
            context
                .toolhost_exe
                .parent()
                .and_then(|p| p.parent())
                .unwrap_or(&context.workdir)
                .to_path_buf()
        });
    let config = GatewayConfig {
        data_home: context
            .runtime
            .model_home
            .get()
            .cloned()
            .unwrap_or_else(|| context.data_home.join("Models")),
        data_root: context.data_home.clone(),
        extension_root: context.extension_root.clone(),
        application_root: root.clone(),
        port: 0,
        toolhost_exe: context.toolhost_exe.clone(),
        vector_extension: root.join("runtime/native/vec0.dll"),
        inference_library: root.join("runtime/native/onnxruntime.dll"),
    };
    let action = arguments["action"]
        .as_str()
        .ok_or_else(|| invalid("缺少训练操作。"))?;
    handle(
        action,
        arguments.get("payload").unwrap_or(&Value::Null),
        context,
        &config,
    )
    .await
}
/// Native picker operations have no model-tool equivalent and do not expand workspace roots.
pub(crate) async fn selected(
    action: &str,
    input: &Value,
    ctx: &GatewayToolContext,
) -> Result<Value, GatewayError> {
    gateway::check_cancelled(ctx)?;
    match action {
        "training.sources.import" => imports::import(input, ctx).await,
        "training.models.register" => imports::register_model(input, ctx),
        "training.environment.configure" => {
            let _lock = CONFIGURATION
                .lock()
                .map_err(|_| invalid("训练配置正在保存。"))?;
            let path = input["path"]
                .as_str()
                .ok_or_else(|| invalid("请选择Python可执行文件。"))?;
            let (path, _) = gateway::canonical_authorized(ctx, path, false)?;
            if !path.is_file()
                || gateway::protected_path(ctx, &path)
                    && !path.starts_with(ctx.data_home.join("Runtime"))
            {
                return Err(invalid("选择的Python环境不可用。"));
            }
            if !path.file_name().and_then(|n| n.to_str()).is_some_and(|n| {
                matches!(
                    n.to_ascii_lowercase().as_str(),
                    "python.exe" | "python3.exe" | "python" | "python3"
                )
            }) {
                return Err(invalid("请选择Python解释器，不是其他可执行程序。"));
            }
            store::write(
                ctx,
                &store::scope_dir(ctx).join("environment.json"),
                &json!({"path":path}),
            )?;
            Ok(json!({"environment":{"pythonPath":path,"configured":true}}))
        }
        _ => Err(invalid("此选择器操作不可用。")),
    }
}
pub(crate) async fn shutdown(home: &std::path::Path) {
    tokio::join!(
        jobs::shutdown(home),
        environment::shutdown(home),
        trial::shutdown(home)
    );
}
pub(crate) async fn shutdown_checked(home: &std::path::Path) -> Result<(), GatewayError> {
    environment::shutdown(home).await;
    trial::shutdown_checked(home).await?;
    jobs::shutdown_checked(home).await
}

// Compile the production download modules directly, including their local HTTP fixtures.
// 直接编译生产下载模块及其本地 HTTP 夹具，不复制下载实现或使用真实模型数据。
pub use kynxa_agent_core_native::{models::{gateway, gateway_config}, storage};
#[allow(dead_code)]
#[path = "../../apps/agent-core/src/models/gateway_download_network.rs"]
mod gateway_download_network;
#[allow(dead_code)]
#[path = "../../apps/agent-core/src/models/gateway_local_models.rs"]
mod local_models;

// Run the production scheduler's focused contracts without starting a model or touching user data.
// 直接验证生产调度约定，不启动模型、不读取用户数据。
pub use kynxa_agent_core_native::models;
#[allow(dead_code)]
#[path = "../../apps/agent-core/src/orchestration/gateway_model_pool.rs"]
mod model_pool;

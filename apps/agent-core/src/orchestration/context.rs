use serde_json::Value;

/// Compatibility entry delegates all context rules to their model-domain owner.
/// 兼容入口把全部上下文规则委托给模型领域拥有者，不复制规则或状态。
pub fn apply(operation: &str, args: &Value) -> Result<Value, String> {
    crate::models::context_control::apply(operation, args)
}

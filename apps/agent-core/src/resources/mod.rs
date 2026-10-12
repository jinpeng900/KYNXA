pub mod admission;
pub mod coordination;
pub mod external;
pub mod external_admission;
pub mod fallback;
pub mod inference;
pub mod reservation;

use serde_json::Value;

/// Demand estimates and fallback rules do not replace the live Rust resource authority.
/// 需求估计与降级规则不替代实时 Rust 资源权威，不自行确认驻留回收或进程占用。
pub fn calculate(operation: &str, arguments: &Value) -> Result<Value, String> {
    if operation.starts_with("resources.external") {
        external::calculate(operation, arguments)
    } else if operation == "resources.inferenceMeasuredRequest" {
        reservation::calculate(operation, arguments)
    } else if operation == "resources.inferenceLimits" {
        admission::calculate(operation, arguments)
    } else if operation.starts_with("resources.inference") {
        inference::calculate(operation, arguments)
    } else {
        fallback::calculate(operation, arguments)
    }
}
pub mod gateway;

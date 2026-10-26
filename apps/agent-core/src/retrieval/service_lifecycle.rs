use serde_json::{Value, json};
use std::collections::{HashMap, VecDeque};

type PolicyResult = Result<Value, Value>;

fn error(message: &str, code: &str, status: u16) -> Value {
    json!({"message":message,"code":code,"statusCode":status})
}
fn number(arguments: &Value, key: &str) -> u64 {
    arguments[key].as_u64().unwrap_or(0)
}

#[derive(Default)]
struct RecoveryWindow {
    times: VecDeque<u64>,
    restarts: u64,
}
impl RecoveryWindow {
    fn available(&mut self, now: u64) -> bool {
        while self
            .times
            .front()
            .is_some_and(|time| *time <= now.saturating_sub(60000))
        {
            self.times.pop_front();
        }
        self.times.len() < 2
    }
    fn record(&mut self, now: u64) {
        self.times.push_back(now);
        self.restarts += 1;
    }
}

/// Track capacity, cache order and recovery windows; hold dispatched capacity until cancellation settles.
/// 维护容量、缓存顺序和恢复窗口，取消结算前保留已派发额度。
#[derive(Default)]
pub struct StructureState {
    cache: VecDeque<(String, u64)>,
    cache_bytes: u64,
    hits: u64,
    tickets: HashMap<u64, u64>,
    queued_bytes: u64,
    recovery: RecoveryWindow,
}
impl StructureState {
    fn apply(&mut self, operation: &str, arguments: &Value) -> PolicyResult {
        let key = arguments["key"].as_str().unwrap_or("");
        match operation {
            "init" | "maxChars" => {
                let (field, minimum, maximum, message) = if operation == "init" {
                    (
                        "timeoutMs",
                        1,
                        30000,
                        "Invalid parser timeout. / 解析期限无效。",
                    )
                } else {
                    (
                        "maxChars",
                        64,
                        8000,
                        "Invalid structure chunk size. / 结构分块大小无效。",
                    )
                };
                let value = arguments[field]
                    .as_u64()
                    .filter(|value| (minimum..=maximum).contains(value));
                value
                    .map(|_| Value::Null)
                    .ok_or_else(|| error(message, "INVALID_RETRIEVAL_INPUT", 400))
            }
            "cacheGet" => {
                if let Some(index) = self.cache.iter().position(|(stored, _)| stored == key) {
                    let entry = self.cache.remove(index).unwrap();
                    self.cache.push_back(entry);
                    self.hits += 1;
                    Ok(json!(true))
                } else {
                    Ok(json!(false))
                }
            }
            "cachePut" => {
                let bytes = number(arguments, "bytes");
                let budget = arguments["leaseMemoryBytes"]
                    .as_u64()
                    .map(|bytes| (bytes / 8).min(64 * 1024 * 1024))
                    .unwrap_or(8 * 1024 * 1024);
                if bytes > budget || arguments["parseStatus"] == "unavailable" {
                    return Ok(json!({"store":false,"evicted":[]}));
                }
                // Replacing an existing Map entry retains its order; only reads refresh the LRU position.
                // 替换现有 Map 项保留插入顺序，只有读取刷新 LRU 位置，以兼容并发解析回执。
                if let Some(index) = self.cache.iter().position(|(stored, _)| stored == key) {
                    self.cache_bytes -= self.cache[index].1;
                    self.cache[index].1 = bytes;
                } else {
                    self.cache.push_back((key.to_owned(), bytes));
                }
                self.cache_bytes += bytes;
                let maximum = if arguments["hasResources"] == true {
                    256
                } else {
                    64
                };
                let mut evicted = Vec::new();
                while self.cache.len() > maximum || self.cache_bytes > budget {
                    let (key, bytes) = self.cache.pop_front().unwrap();
                    self.cache_bytes -= bytes;
                    evicted.push(key);
                }
                Ok(json!({"store":true,"evicted":evicted}))
            }
            "clearCache" => {
                self.cache.clear();
                self.cache_bytes = 0;
                Ok(Value::Null)
            }
            "admit" => {
                let maximum = if arguments["hasResources"] == true {
                    128
                } else {
                    32
                };
                let budget = arguments["leaseMemoryBytes"]
                    .as_u64()
                    .unwrap_or(64 * 1024 * 1024);
                if self.tickets.len() >= maximum
                    || self.queued_bytes.saturating_add(number(arguments, "bytes")) > budget / 2
                {
                    Err(error(
                        "Source parser queue is full. / 来源解析队列已满。",
                        "STRUCTURE_QUEUE_FULL",
                        503,
                    ))
                } else {
                    Ok(Value::Null)
                }
            }
            "ticketAdd" => {
                let id = number(arguments, "id");
                let bytes = number(arguments, "bytes");
                if let Some(previous) = self.tickets.insert(id, bytes) {
                    self.queued_bytes -= previous;
                }
                self.queued_bytes += bytes;
                Ok(Value::Null)
            }
            "ticketRelease" => {
                if let Some(bytes) = self.tickets.remove(&number(arguments, "id")) {
                    self.queued_bytes -= bytes;
                    Ok(json!(true))
                } else {
                    Ok(json!(false))
                }
            }
            "ticketIds" => Ok(json!(self.tickets.keys().copied().collect::<Vec<_>>())),
            "recoverCheck" => Ok(json!(self.recovery.available(number(arguments, "now")))),
            "recoverComplete" => {
                self.recovery.record(number(arguments, "now"));
                Ok(Value::Null)
            }
            "stats" => Ok(
                json!({"cacheEntries":self.cache.len(),"cacheBytes":self.cache_bytes,"cacheHits":self.hits,"queuedBytes":self.queued_bytes,"workerRestarts":self.recovery.restarts}),
            ),
            _ => Err(error(
                "Unknown parser lifecycle operation.",
                "AGENT_CORE_UNKNOWN_OPERATION",
                400,
            )),
        }
    }
}

#[derive(Default)]
pub struct TransportState {
    recovery: RecoveryWindow,
    search_timeouts: u64,
    hard_retirements: u64,
}
impl TransportState {
    fn apply(&mut self, operation: &str, arguments: &Value) -> PolicyResult {
        match operation {
            "admit" => {
                if number(arguments, "pending") >= 256 {
                    Err(error(
                        "Retrieval queue is full. / 检索队列已满。",
                        "RETRIEVAL_QUEUE_FULL",
                        503,
                    ))
                } else {
                    Ok(Value::Null)
                }
            }
            "recover" => {
                let now = number(arguments, "now");
                let available = self.recovery.available(now)
                    && arguments["closed"] != true
                    && arguments["closing"] != true
                    && arguments["retirementFailed"] != true;
                if available {
                    self.recovery.record(now);
                }
                Ok(json!(available))
            }
            "timeout" => {
                self.search_timeouts += 1;
                Ok(Value::Null)
            }
            "retired" => {
                self.hard_retirements += 1;
                Ok(Value::Null)
            }
            "stats" => Ok(
                json!({"workerRestarts":self.recovery.restarts,"searchTimeouts":self.search_timeouts,"hardSearchRetirements":self.hard_retirements}),
            ),
            _ => Err(error(
                "Unknown retrieval transport operation.",
                "AGENT_CORE_UNKNOWN_OPERATION",
                400,
            )),
        }
    }
}

pub fn structure(state: &mut StructureState, operation: &str, arguments: &Value) -> Value {
    match state.apply(operation, arguments) {
        Ok(value) => json!({"value":value}),
        Err(error) => json!({"error":error}),
    }
}
pub fn transport(state: &mut TransportState, operation: &str, arguments: &Value) -> Value {
    match state.apply(operation, arguments) {
        Ok(value) => json!({"value":value}),
        Err(error) => json!({"error":error}),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn cache_read_order_and_bytes_control_eviction() {
        let mut state = StructureState::default();
        state
            .apply(
                "cachePut",
                &json!({"key":"a","bytes":4,"leaseMemoryBytes":64}),
            )
            .unwrap();
        state
            .apply(
                "cachePut",
                &json!({"key":"b","bytes":4,"leaseMemoryBytes":64}),
            )
            .unwrap();
        assert_eq!(state.apply("cacheGet", &json!({"key":"a"})).unwrap(), true);
        let receipt = state
            .apply(
                "cachePut",
                &json!({"key":"c","bytes":4,"leaseMemoryBytes":64}),
            )
            .unwrap();
        assert_eq!(receipt["evicted"], json!(["b"]));
        assert_eq!(state.apply("stats", &json!({})).unwrap()["cacheBytes"], 8);
    }
    #[test]
    fn acknowledgement_and_strict_restart_window_enforce_capacity() {
        let mut state = StructureState::default();
        state
            .apply("ticketAdd", &json!({"id":1,"bytes":32*1024*1024}))
            .unwrap();
        assert!(state.apply("admit", &json!({"bytes":1})).is_err());
        state.apply("ticketRelease", &json!({"id":1})).unwrap();
        assert!(state.apply("admit", &json!({"bytes":1})).is_ok());
        let mut transport = TransportState::default();
        assert_eq!(
            transport.apply("recover", &json!({"now":100000})).unwrap(),
            true
        );
        assert_eq!(
            transport.apply("recover", &json!({"now":100001})).unwrap(),
            true
        );
        assert_eq!(
            transport.apply("recover", &json!({"now":159999})).unwrap(),
            false
        );
        assert_eq!(
            transport.apply("recover", &json!({"now":160000})).unwrap(),
            true
        );
        assert!(transport.apply("admit", &json!({"pending":256})).is_err());
    }
}

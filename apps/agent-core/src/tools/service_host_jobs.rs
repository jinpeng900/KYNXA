use std::collections::{HashMap, VecDeque};

use serde_json::{Value, json};

use super::local_files::random_identity;
use super::service_error::{ServiceError, ServiceResult};
use crate::models::protocol::{array, string};

const OUTPUT_UNITS: usize = 262144;
fn running(status: &Value) -> bool {
    ["starting", "running"].contains(&string(status))
}
fn bounded(value: &Value, fallback: u64, minimum: u64, maximum: u64) -> ServiceResult<u64> {
    if value.is_null() {
        return Ok(fallback);
    }
    value
        .as_u64()
        .filter(|value| (minimum..=maximum).contains(value))
        .ok_or_else(|| {
            ServiceError::new(
                "INVALID_TOOL_ARGUMENTS",
                &format!("整数参数须在 {minimum}–{maximum} 范围内。"),
                400,
            )
        })
}
struct HostJob {
    metadata: Value,
    output: Vec<u16>,
    total: usize,
    base: usize,
}

impl HostJob {
    fn view(&self) -> Value {
        let mut value = self.metadata.clone();
        value["running"] = json!(running(&value["status"]));
        value["totalCharacters"] = json!(self.total);
        value["earliestOffset"] = json!(self.base);
        value
    }
}

/// Terminal jobs retain only bounded output and formal receipts, independently of request completion.
/// 终端作业独立于请求完成，保留受限输出与正式回执。
#[derive(Default)]
pub struct HostJobs {
    jobs: HashMap<String, HostJob>,
    order: VecDeque<String>,
    maximum: f64,
    closed: bool,
}

impl HostJobs {
    fn get(&self, arguments: &Value) -> ServiceResult<&HostJob> {
        let job = self.jobs.get(string(&arguments["jobId"])).ok_or_else(|| {
            ServiceError::new(
                "HOST_TERMINAL_JOB_NOT_FOUND",
                "当前聊天中没有此终端任务。",
                404,
            )
        })?;
        if job.metadata["conversationId"] != arguments["context"]["conversationId"] {
            return Err(ServiceError::new(
                "HOST_TERMINAL_JOB_NOT_FOUND",
                "当前聊天中没有此终端任务。",
                404,
            ));
        }
        Ok(job)
    }
    pub fn apply(&mut self, operation: &str, arguments: &Value) -> ServiceResult<Value> {
        match operation {
            "host.init" => {
                self.maximum = arguments["maximumRunning"].as_f64().unwrap_or(4.0);
                Ok(Value::Null)
            }
            "host.start" => {
                if self.closed {
                    return Err(ServiceError::new(
                        "HOST_TERMINAL_UNAVAILABLE",
                        "后台终端任务服务已经关闭。",
                        503,
                    ));
                }
                if string(&arguments["context"]["conversationId"]).is_empty() {
                    return Err(ServiceError::new(
                        "HOST_TERMINAL_INVALID_REQUEST",
                        "后台终端任务需要聊天身份。",
                        400,
                    ));
                }
                if self
                    .jobs
                    .values()
                    .filter(|job| running(&job.metadata["status"]))
                    .count() as f64
                    >= self.maximum
                {
                    return Err(ServiceError::new(
                        "HOST_TERMINAL_CAPACITY",
                        "后台终端任务已达到并发上限。",
                        429,
                    ));
                }
                let mut evicted = Vec::new();
                for id in self.order.clone() {
                    if self.jobs.len() < 32 {
                        break;
                    }
                    if self
                        .jobs
                        .get(&id)
                        .is_some_and(|job| !running(&job.metadata["status"]))
                    {
                        self.jobs.remove(&id);
                        self.order.retain(|key| *key != id);
                        evicted.push(id);
                    }
                }
                let timeout = bounded(&arguments["request"]["timeoutMs"], 1800000, 100, 21600000)?;
                let id = random_identity()?;
                let mut metadata = json!({"jobId":id,"conversationId":arguments["context"]["conversationId"],"status":"starting","startedAt":arguments["startedAt"],"timeoutMs":timeout});
                for field in ["shell", "cwd"] {
                    if let Some(value) = arguments["request"].get(field) {
                        metadata[field] = value.clone();
                    }
                }
                self.jobs.insert(
                    id.clone(),
                    HostJob {
                        metadata,
                        output: Vec::new(),
                        total: 0,
                        base: 0,
                    },
                );
                self.order.push_back(id.clone());
                Ok(json!({"jobId":id,"timeoutMs":timeout,"evicted":evicted,
                "startupTimeoutMs":15000,"startupFailure":{"message":"Background terminal startup acknowledgement timed out."},
                "startupCancellation":{"name":"AbortError","message":"后台终端任务启动已取消。"}}))
            }
            "host.field" => Ok(self
                .jobs
                .get(string(&arguments["jobId"]))
                .map(|job| job.metadata[string(&arguments["field"])].clone())
                .unwrap_or(Value::Null)),
            "host.install" => {
                let id = string(&arguments["jobId"]);
                let mut metadata = arguments["metadata"].clone();
                metadata["jobId"] = json!(id);
                let result = &arguments["result"];
                if !result.is_null() {
                    let mut receipt = result["value"].as_object().cloned().unwrap_or_default();
                    for field in ["stdout", "stderr", "consoleText"] {
                        receipt.remove(field);
                    }
                    for field in ["isError", "code", "status"] {
                        if let Some(value) = result.get(field) {
                            receipt.insert(field.into(), value.clone());
                        }
                    }
                    metadata["receipt"] = Value::Object(receipt);
                }
                let output = array(&arguments["units"])
                    .iter()
                    .filter_map(Value::as_u64)
                    .map(|unit| unit as u16)
                    .collect();
                if !self.jobs.contains_key(id) {
                    self.order.push_back(id.into());
                }
                self.jobs.insert(
                    id.into(),
                    HostJob {
                        metadata,
                        output,
                        total: arguments["totalCharacters"].as_u64().unwrap_or(0) as usize,
                        base: arguments["baseOffset"].as_u64().unwrap_or(0) as usize,
                    },
                );
                Ok(Value::Null)
            }
            "host.patch" => {
                if let Some(job) = self.jobs.get_mut(string(&arguments["jobId"])) {
                    if let Some(fields) = arguments["fields"].as_object() {
                        for (key, value) in fields {
                            job.metadata[key] = value.clone();
                        }
                    }
                }
                Ok(Value::Null)
            }
            "host.started" => {
                let mut acknowledged = false;
                if let Some(job) = self.jobs.get_mut(string(&arguments["jobId"])) {
                    if job.metadata["status"] == "starting"
                        && arguments["processId"]
                            .as_u64()
                            .is_some_and(|id| id > 0 && id <= 9_007_199_254_740_991)
                    {
                        job.metadata["processId"] = arguments["processId"].clone();
                        job.metadata["status"] = json!("running");
                        acknowledged = true;
                    }
                }
                Ok(json!(acknowledged))
            }
            "host.append" => {
                if let Some(job) = self.jobs.get_mut(string(&arguments["jobId"])) {
                    if arguments["replace"] != true {
                        let units = array(&arguments["units"])
                            .iter()
                            .filter_map(Value::as_u64)
                            .map(|unit| unit as u16)
                            .collect::<Vec<_>>();
                        job.total += units.len();
                        job.output.extend(units);
                        if job.output.len() > OUTPUT_UNITS {
                            job.output.drain(..job.output.len() - OUTPUT_UNITS);
                        }
                        if job
                            .output
                            .first()
                            .is_some_and(|unit| (0xdc00..=0xdfff).contains(unit))
                        {
                            job.output.remove(0);
                        }
                        job.base = job.total - job.output.len();
                    }
                }
                Ok(Value::Null)
            }
            "host.complete" => {
                let id = string(&arguments["jobId"]);
                let job = self.jobs.get_mut(id).ok_or_else(|| {
                    ServiceError::new(
                        "HOST_TERMINAL_JOB_NOT_FOUND",
                        "当前聊天中没有此终端任务。",
                        404,
                    )
                })?;
                let result = &arguments["result"];
                let mut receipt = result["value"].as_object().cloned().unwrap_or_default();
                for field in ["stdout", "stderr", "consoleText"] {
                    receipt.remove(field);
                }
                for field in ["isError", "code", "status"] {
                    if let Some(value) = result.get(field) {
                        receipt.insert(field.into(), value.clone());
                    }
                }
                job.metadata["receipt"] = Value::Object(receipt);
                job.metadata["status"] = json!(if result["status"] == "unknown" {
                    "unknown"
                } else if result["isError"] == true {
                    "error"
                } else {
                    "completed"
                });
                if job.metadata["processId"].is_null() {
                    return Ok(
                        json!({"failure":ServiceError::new("HOST_TERMINAL_INVALID_RESULT",&format!("后台终端任务没有可核验的启动确认，请用 terminal.host.read 查询任务 {id}，不要自动重新执行命令。"),502).with_details(json!({"jobId":id,"outcomeUnknown":result["status"]=="unknown"||result["value"]["completed"]==true})).value()}),
                    );
                }
                Ok(Value::Null)
            }
            "host.failed" => {
                if let Some(job) = self.jobs.get_mut(string(&arguments["jobId"])) {
                    job.metadata["status"] =
                        json!(if arguments["failure"]["name"] == "AbortError" {
                            "cancelled"
                        } else {
                            "error"
                        });
                    job.metadata["error"] = json!({"code":arguments["failure"]["code"].as_str().unwrap_or("HOST_TERMINAL_FAILED"),"message":arguments["failure"]["message"]});
                }
                Ok(Value::Null)
            }
            "host.view" | "host.read" | "host.stop" => {
                let job = self.get(arguments)?;
                let mut value = job.view();
                value.as_object_mut().unwrap().remove("conversationId");
                if operation == "host.stop" {
                    return Ok(json!({"abort":running(&job.metadata["status"])}));
                }
                if operation == "host.view" {
                    return Ok(value);
                }
                let offset = bounded(&arguments["offset"], 0, 0, 9_007_199_254_740_991)? as usize;
                let limit = bounded(&arguments["limit"], 16000, 1, 64000)? as usize;
                if offset > job.total {
                    return Err(ServiceError::new(
                        "HOST_TERMINAL_INVALID_OFFSET",
                        "输出位置超出终端任务记录。",
                        400,
                    ));
                }
                let actual = offset.max(job.base);
                let start = actual - job.base;
                if start > 0
                    && job
                        .output
                        .get(start - 1)
                        .is_some_and(|unit| (0xd800..=0xdbff).contains(unit))
                    && job
                        .output
                        .get(start)
                        .is_some_and(|unit| (0xdc00..=0xdfff).contains(unit))
                {
                    return Err(ServiceError::new(
                        "HOST_TERMINAL_INVALID_OFFSET",
                        "输出位置不能位于字符代理对中间，请使用上一页的 nextOffset。",
                        400,
                    ));
                }
                let mut next = job.total.min(actual + limit);
                let end = next - job.base;
                if end < job.output.len()
                    && end > 0
                    && (0xd800..=0xdbff).contains(&job.output[end - 1])
                    && (0xdc00..=0xdfff).contains(&job.output[end])
                {
                    if next - actual == 1 {
                        next += 1;
                    } else {
                        next -= 1;
                    }
                }
                value["outputUnits"] = json!(job.output.get(start..next - job.base).unwrap_or(&[]));
                value["offset"] = json!(actual);
                value["nextOffset"] = json!(next);
                value["hasMore"] = json!(next < job.total);
                value["truncated"] = json!(offset < job.base);
                Ok(value)
            }
            "host.close" => {
                self.closed = true;
                Ok(json!(
                    self.order
                        .iter()
                        .filter(|id| self
                            .jobs
                            .get(*id)
                            .is_some_and(|job| running(&job.metadata["status"])))
                        .collect::<Vec<_>>()
                ))
            }
            _ => Err(ServiceError::new(
                "AGENT_CORE_UNKNOWN_OPERATION",
                "Unknown host terminal job operation.",
                400,
            )),
        }
    }
}

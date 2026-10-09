mod admission;
mod budget;
mod gpu;
mod gpu_mapping;
mod policy;
mod sampling;

use admission::AdmissionQueue;
use budget::{AllocationRequest, ResourceBudget, RestoredLease};
use policy::WorkFeedback;
use sampling::HardwareSampler;
use serde::Deserialize;
use serde_json::{Value, json};
use std::collections::VecDeque;
use std::io::{self, BufRead, Read, Write};
use std::sync::mpsc::{self, RecvTimeoutError};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

const MAX_FRAME_BYTES: usize = 65_536;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Request {
    id: u64,
    method: String,
    #[serde(default)]
    params: Value,
}

fn epoch_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

fn respond(
    output: &mut impl Write,
    id: u64,
    mut result: Value,
    retired: &VecDeque<String>,
) -> io::Result<()> {
    result["retiredLeaseIds"] = json!(retired);
    serde_json::to_writer(&mut *output, &json!({"id":id,"result":result}))?;
    writeln!(output)?;
    output.flush()
}

fn main() {
    let (sender, receiver) = mpsc::sync_channel::<Vec<u8>>(16);
    // Only stdin is read; owner EOF closes the service, without inspecting other applications.
    // 仅从拥有者的 stdin 接收消息，EOF 即退出；不扫描其他软件的命令行或私有内容。
    std::thread::spawn(move || {
        let input = io::stdin();
        let mut input = input.lock();
        loop {
            let mut frame = Vec::new();
            match input
                .by_ref()
                .take((MAX_FRAME_BYTES + 1) as u64)
                .read_until(b'\n', &mut frame)
            {
                Ok(0) | Err(_) => break,
                Ok(_) if frame.len() > MAX_FRAME_BYTES => break,
                Ok(_) => {
                    if sender.send(frame).is_err() {
                        break;
                    }
                }
            }
        }
    });
    let mut sampler = HardwareSampler::new();
    let mut budget = ResourceBudget::new();
    let mut admission = AdmissionQueue::new();
    let mut retired_leases = VecDeque::<String>::new();
    let arguments: Vec<String> = std::env::args().collect();
    let owner_pid = arguments
        .iter()
        .position(|argument| argument == "--owner-pid")
        .and_then(|index| arguments.get(index + 1))
        .and_then(|argument| argument.parse::<u32>().ok())
        .unwrap_or(0);
    let output = io::stdout();
    let mut output = output.lock();
    loop {
        let frame = match receiver.recv_timeout(Duration::from_millis(if admission.len() > 0 {
            25
        } else {
            1000
        })) {
            Ok(frame) => Some(frame),
            Err(RecvTimeoutError::Timeout) => None,
            Err(RecvTimeoutError::Disconnected) => break,
        };
        let now_ms = epoch_ms();
        let hardware = sampler.snapshot(budget.has_gpu_allocation());
        budget.reconcile_memory(sampler.memory_observations());
        budget.reconcile_gpu_memory(sampler.gpu_memory_observations());
        for lease_id in sampler.retired_executors() {
            budget.release(&lease_id);
            sampler.remove_executor(&lease_id);
            retired_leases.push_back(lease_id);
            while retired_leases.len() > 128 {
                retired_leases.pop_front();
            }
        }
        let mut is_failed = false;
        for (id, outcome) in admission.drain(&mut budget, &hardware, now_ms) {
            if respond(&mut output, id, outcome, &retired_leases).is_err() {
                is_failed = true;
                break;
            }
        }
        if is_failed {
            break;
        }
        let Some(frame) = frame else {
            continue;
        };
        let request: Request = match serde_json::from_slice(&frame) {
            Ok(request) => request,
            Err(_) => break,
        };
        let needs_gpu =
            budget.has_gpu_allocation()
                || request.method == "acquire"
                    && request.params["gpuMemoryBytes"]
                        .as_u64()
                        .is_some_and(|bytes| bytes > 0);
        let hardware = if request.method == "reconcile" { sampler.reconcile(needs_gpu) } else { sampler.snapshot(needs_gpu) };
        budget.reconcile_memory(sampler.memory_observations());
        budget.reconcile_gpu_memory(sampler.gpu_memory_observations());
        let result = match request.method.as_str() {
            "snapshot" | "health" | "reconcile" => {
                for lease_id in sampler.retired_executors() {
                    budget.release(&lease_id);
                    sampler.remove_executor(&lease_id);
                    retired_leases.push_back(lease_id);
                    while retired_leases.len() > 128 { retired_leases.pop_front(); }
                }
                let mut snapshot = budget.snapshot(&hardware, now_ms);
                snapshot["queuedRequests"] = json!(admission.len());
                snapshot["executors"] = sampler.executor_snapshot();
                if request.method == "reconcile" { snapshot["status"] = json!("reconciled"); }
                Some(snapshot)
            }
            "restore" => Some(match serde_json::from_value::<Vec<RestoredLease>>(request.params["leases"].clone()) {
                Ok(leases) => budget.restore(leases),
                Err(_) => json!({"status":"denied","reason":"RESOURCE_RESTORE_INVALID"}),
            }),
            "acquire" => match serde_json::from_value::<AllocationRequest>(request.params) {
                Ok(allocation) => {
                    let outcome = budget.acquire(allocation.clone(), &hardware, now_ms);
                    if allocation.wait_ms > 0 && outcome["reason"] == "RESOURCE_PRESSURE" {
                        admission.enqueue(request.id, allocation, now_ms)
                    } else {
                        Some(outcome)
                    }
                }
                Err(_) => Some(json!({"status":"denied","reason":"RESOURCE_INVALID_REQUEST"})),
            },
            "cancelAcquire" => {
                if let Some((id, cancelled)) =
                    admission.cancel(request.params["requestId"].as_u64().unwrap_or(0))
                {
                    if respond(&mut output, id, cancelled, &retired_leases).is_err() {
                        break;
                    }
                }
                Some(json!({"status":"cancelled"}))
            }
            "registerExecutor" => {
                let lease_id = request.params["leaseId"].as_str().unwrap_or("");
                if !budget.contains_lease(lease_id) {
                    Some(json!({"status":"denied","reason":"RESOURCE_LEASE_UNKNOWN"}))
                } else {
                    Some(sampler.register_executor(
                        lease_id.to_string(),
                        request.params["processId"].as_u64().unwrap_or(0) as u32,
                        request.params["startTimeMs"].as_u64(),
                        owner_pid,
                    ))
                }
            }
            "recoverExecutor" => {
                let lease_id = request.params["leaseId"].as_str().unwrap_or("");
                Some(if !budget.contains_lease(lease_id) { json!({"status":"denied","reason":"RESOURCE_LEASE_UNKNOWN"}) }
                    else {sampler.recover_executor(lease_id.to_string(),request.params["processId"].as_u64().unwrap_or(0) as u32,
                        request.params["startTimeMs"].as_u64().unwrap_or(0),owner_pid)})
            }
            "report" => {
                let lease_id = request.params["leaseId"].as_str().unwrap_or("");
                Some(
                    match serde_json::from_value::<WorkFeedback>(request.params["feedback"].clone())
                    {
                        Ok(feedback) => budget.report(lease_id, feedback, now_ms),
                        Err(_) => json!({"status":"denied","reason":"RESOURCE_INVALID_FEEDBACK"}),
                    },
                )
            }
            "renew" => Some(budget.renew(
                request.params["leaseId"].as_str().unwrap_or(""),
                request.params["ttlMs"].as_u64().unwrap_or(30_000),
                now_ms,
            )),
            "release" => {
                let lease_id = request.params["leaseId"].as_str().unwrap_or("");
                sampler.remove_executor(lease_id);
                Some(budget.release(lease_id))
            }
            "close" => Some(json!({ "status": "closed" })),
            _ => Some(json!({ "status": "denied", "reason": "RESOURCE_UNKNOWN_METHOD" })),
        };
        if let Some(result) = result {
            if respond(&mut output, request.id, result, &retired_leases).is_err() {
                break;
            }
        }
        if request.method == "close" {
            break;
        }
    }
}

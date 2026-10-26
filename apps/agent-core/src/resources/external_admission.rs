use std::collections::HashMap;

use serde_json::{Value, json};

#[derive(Default)]
struct Entry {
    users: u64,
    waiters: u64,
    in_flight: u64,
    quarantined: bool,
    finishing: bool,
    releasing: bool,
    renewing: bool,
    snapshot: Value,
    plan: Value,
    state: Value,
    coordination: Value,
    lease_id: Option<String>,
}

#[derive(Default)]
struct Borrower {
    key: String,
    pending: bool,
    released: bool,
}

/// Keep one shared prediction ledger; callers retain live runtime objects only.
/// 共用一份预测账本，调用方仅持有活动运行时对象。
#[derive(Default)]
pub struct ExternalAdmissionState {
    entries: HashMap<String, Entry>,
    borrowers: HashMap<u64, Borrower>,
    sequence: u64,
    closed: bool,
}

impl ExternalAdmissionState {
    pub fn apply(&mut self, operation: &str, args: &Value) -> Result<Value, String> {
        let key = args["key"].as_str().unwrap_or("");
        match operation {
            "resources.externalAdmission.open" => {
                if self.closed {
                    return Err("RESOURCE_EXTERNAL_COORDINATION_CLOSED".into());
                }
                if self.entries.contains_key(key) {
                    return Ok(json!({"created": false}));
                }
                if self.entries.len() >= 32 {
                    return Err("RESOURCE_EXTERNAL_COORDINATION_CAPACITY".into());
                }
                self.entries.insert(key.into(), Entry::default());
                Ok(json!({"created": true}))
            }
            "resources.externalAdmission.join" => {
                if self.closed {
                    return Err("RESOURCE_EXTERNAL_COORDINATION_CLOSED".into());
                }
                let entry = self
                    .entries
                    .get_mut(key)
                    .ok_or("RESOURCE_EXTERNAL_ALLOCATION_UNCONFIRMED")?;
                if entry.quarantined {
                    return Err("RESOURCE_EXTERNAL_ALLOCATION_UNCONFIRMED".into());
                }
                if entry.finishing || entry.releasing {
                    return Ok(json!({"wait": true}));
                }
                entry.waiters += 1;
                Ok(json!({"wait": false}))
            }
            "resources.externalAdmission.leave" => {
                if let Some(entry) = self.entries.get_mut(key) {
                    entry.waiters = entry.waiters.saturating_sub(1);
                }
                Ok(Value::Null)
            }
            "resources.externalAdmission.admit" => {
                if self.closed {
                    return Err("RESOURCE_EXTERNAL_ALLOCATION_UNCONFIRMED".into());
                }
                let entry = self
                    .entries
                    .get_mut(key)
                    .ok_or("RESOURCE_EXTERNAL_ALLOCATION_UNCONFIRMED")?;
                if entry.quarantined {
                    return Err("RESOURCE_EXTERNAL_ALLOCATION_UNCONFIRMED".into());
                }
                self.sequence = self
                    .sequence
                    .checked_add(1)
                    .ok_or("RESOURCE_EXTERNAL_COORDINATION_CAPACITY")?;
                entry.users += 1;
                self.borrowers.insert(
                    self.sequence,
                    Borrower {
                        key: key.into(),
                        ..Borrower::default()
                    },
                );
                Ok(
                    json!({"borrower": self.sequence, "snapshot": entry.snapshot, "plan": entry.plan}),
                )
            }
            "resources.externalAdmission.dispatched"
            | "resources.externalAdmission.settled"
            | "resources.externalAdmission.releaseBorrower" => {
                let id = args["borrower"]
                    .as_u64()
                    .ok_or("RESOURCE_EXTERNAL_ALLOCATION_UNCONFIRMED")?;
                let Some(borrower) = self.borrowers.get_mut(&id) else {
                    return if operation.ends_with("dispatched") {
                        Err("RESOURCE_EXTERNAL_ALLOCATION_UNCONFIRMED".into())
                    } else {
                        Ok(json!(false))
                    };
                };
                let Some(entry) = self.entries.get_mut(&borrower.key) else {
                    return if operation.ends_with("dispatched") {
                        Err("RESOURCE_EXTERNAL_ALLOCATION_UNCONFIRMED".into())
                    } else {
                        Ok(json!(false))
                    };
                };
                if operation.ends_with("dispatched") {
                    if borrower.released || borrower.pending || entry.quarantined || self.closed {
                        return Err("RESOURCE_EXTERNAL_ALLOCATION_UNCONFIRMED".into());
                    }
                    borrower.pending = true;
                    entry.in_flight += 1;
                } else if operation.ends_with("settled") {
                    if borrower.pending {
                        borrower.pending = false;
                        entry.in_flight = entry.in_flight.saturating_sub(1);
                    }
                } else {
                    if borrower.released {
                        return Ok(json!(false));
                    }
                    borrower.released = true;
                    entry.users = entry.users.saturating_sub(1);
                }
                let retired = borrower.released && !borrower.pending;
                if retired {
                    self.borrowers.remove(&id);
                }
                Ok(json!(true))
            }
            "resources.externalAdmission.observed" => {
                let entry = self
                    .entries
                    .get_mut(key)
                    .ok_or("RESOURCE_EXTERNAL_ALLOCATION_UNCONFIRMED")?;
                entry.snapshot = args["snapshot"].clone();
                Ok(json!(entry.snapshot["backend"] == "ollama"))
            }
            "resources.externalAdmission.forecast" => {
                let entry = self
                    .entries
                    .get_mut(key)
                    .ok_or("RESOURCE_EXTERNAL_ALLOCATION_UNCONFIRMED")?;
                let context = entry
                    .snapshot
                    .get("runtimeContextTokens")
                    .filter(|v| v.as_f64().is_some_and(|n| n > 0.0))
                    .unwrap_or(&entry.snapshot["configuredContextTokens"]);
                entry.plan = super::external::demand(
                    &json!({"snapshot": entry.snapshot, "contextTokens": context, "hardware": args["hardware"]}),
                );
                let gpu = entry.plan["gpuMemoryBytes"].as_f64().unwrap_or(0.0);
                let yield_idle = entry.snapshot["observedGpuMemoryBytes"]
                    .as_f64()
                    .unwrap_or(0.0)
                    > 0.0
                    || gpu > 0.0;
                if entry.plan["requiresAdmission"] != true {
                    entry.state = json!({"state": if entry.plan["state"] == "ready" {"no-pending-increment"} else {"unconfirmed"}, "plan": entry.plan});
                    return Ok(json!({"yield": yield_idle, "request": null}));
                }
                if gpu > 0.0 && args["hardware"]["gpu"]["mappingStatus"] != "verified" {
                    entry.state = json!({"state":"unconfirmed", "reason":"GPU_DEVICE_MAPPING_UNVERIFIED", "plan":entry.plan});
                    return Ok(json!({"yield":yield_idle,"request":null}));
                }
                let memory = if gpu > 0.0
                    && entry.plan["breakdown"]["memoryIncludesTransientStaging"] == true
                {
                    json!(0)
                } else {
                    entry.plan["memoryBytes"].clone()
                };
                let model = args["modelId"].as_str().unwrap_or("");
                let task: String = format!("external:{model}").chars().take(128).collect();
                Ok(
                    json!({"yield":yield_idle,"request":{"taskId":task,"workspaceId":"local-external","kind":"foreground","cpuThreads":0,"memoryBytes":memory,"gpuMemoryBytes":gpu,"ttlMs":30000,"waitMs":5000}}),
                )
            }
            "resources.externalAdmission.coordinated" => {
                let entry = self
                    .entries
                    .get_mut(key)
                    .ok_or("RESOURCE_EXTERNAL_ALLOCATION_UNCONFIRMED")?;
                entry.coordination = json!({"state":"owned-idle-gpu-yield-requested","externalModelControlled":false,"receipt":args["receipt"],
                    "globalGenerationState":entry.snapshot.get("globalGenerationState").unwrap_or(&json!("unknown")),
                    "applicationGenerationState":entry.snapshot.get("applicationGenerationState").unwrap_or(&json!("idle"))});
                Ok(Value::Null)
            }
            "resources.externalAdmission.acquired" => {
                let entry = self
                    .entries
                    .get_mut(key)
                    .ok_or("RESOURCE_EXTERNAL_ALLOCATION_UNCONFIRMED")?;
                entry.lease_id = Some(
                    args["leaseId"]
                        .as_str()
                        .ok_or("RESOURCE_EXTERNAL_ALLOCATION_UNCONFIRMED")?
                        .into(),
                );
                let unknown_kv = entry.plan["unknownComponents"]
                    .as_array()
                    .is_some_and(|v| v.iter().any(|v| v == "kv-cache"));
                let memory = &args["request"]["memoryBytes"];
                entry.state = json!({"state":if unknown_kv {"admitted-known-weights-kv-unconfirmed"} else if entry.plan["partialCoverage"]==true {"admitted-predicted-increment-partial"} else {"admitted-predicted-increment"},
                    "plan":entry.plan,"gpuMemoryBytes":args["request"]["gpuMemoryBytes"],"memoryBytes":memory,
                    "hostMemoryAdmission":if memory.as_f64()==Some(0.0)&&entry.plan["memoryBytes"].as_f64().unwrap_or(0.0)>0.0 {"advisory-transient-staging"} else {"predicted-increment"},"observedResidencyReservedAgain":false});
                Ok(Value::Null)
            }
            "resources.externalAdmission.inspect" => {
                let entry = self
                    .entries
                    .get(key)
                    .ok_or("RESOURCE_EXTERNAL_ALLOCATION_UNCONFIRMED")?;
                Ok(json!({"waiters":entry.waiters,"quarantined":entry.quarantined}))
            }
            "resources.externalAdmission.state" => {
                let entry = self
                    .entries
                    .get(key)
                    .ok_or("RESOURCE_EXTERNAL_ALLOCATION_UNCONFIRMED")?;
                let mut state = entry.state.as_object().cloned().unwrap_or_default();
                state.insert("snapshot".into(), entry.snapshot.clone());
                if !entry.coordination.is_null() {
                    state.insert("coordination".into(), entry.coordination.clone());
                }
                Ok(Value::Object(state))
            }
            "resources.externalAdmission.renewStart" => {
                let Some(entry) = self.entries.get_mut(key) else {
                    return Ok(json!(false));
                };
                if entry.renewing || entry.releasing {
                    return Ok(json!(false));
                }
                entry.renewing = true;
                Ok(json!(true))
            }
            "resources.externalAdmission.renewed" => {
                if let Some(entry) = self.entries.get_mut(key) {
                    entry.renewing = false;
                    if args["confirmed"] != true {
                        entry.quarantined = true;
                        entry.state =
                            json!({"state":"quarantined","reason":"resource-renewal-unconfirmed"});
                    }
                }
                Ok(args["confirmed"].clone())
            }
            "resources.externalAdmission.finishStart" => {
                let Some(entry) = self.entries.get_mut(key) else {
                    return Ok(json!(false));
                };
                if entry.users > 0 || entry.waiters > 0 || entry.finishing {
                    return Ok(json!(false));
                }
                entry.finishing = true;
                Ok(json!(true))
            }
            "resources.externalAdmission.finishDisposition" => {
                let Some(entry) = self.entries.get_mut(key) else {
                    return Ok(json!("none"));
                };
                if entry.users > 0 || entry.waiters > 0 {
                    return Ok(json!("none"));
                }
                if entry.in_flight == 0 || entry.lease_id.is_none() {
                    return Ok(json!("release"));
                }
                entry.quarantined = true;
                entry.state =
                    json!({"state":"quarantined","reason":"external-request-outcome-unconfirmed"});
                Ok(json!("reconcile"))
            }
            "resources.externalAdmission.finishEnd" => {
                if let Some(entry) = self.entries.get_mut(key) {
                    entry.finishing = false;
                }
                Ok(Value::Null)
            }
            "resources.externalAdmission.releaseStart" => {
                let Some(entry) = self.entries.get_mut(key) else {
                    return Ok(json!(false));
                };
                if entry.releasing {
                    return Ok(json!(false));
                }
                entry.releasing = true;
                Ok(json!(true))
            }
            "resources.externalAdmission.released" => {
                let Some(mut entry) = self.entries.remove(key) else {
                    return Ok(Value::Null);
                };
                if args["confirmed"] != true {
                    entry.releasing = false;
                    entry.quarantined = true;
                    entry.state =
                        json!({"state":"quarantined","reason":"reservation-release-unconfirmed"});
                    self.entries.insert(key.into(), entry);
                    return Ok(Value::Null);
                }
                // A retired borrower cannot dispatch again; settled callbacks remain harmless.
                // 退役借用者不能再次派发；晚到的结束回调保持幂等。
                self.borrowers.retain(|_, borrower| borrower.key != key);
                entry.state = json!({"state":"prediction-fence-released","reason":args["reason"],"externalResidentOwnership":false});
                let mut state = entry.state.as_object().unwrap().clone();
                state.insert("snapshot".into(), entry.snapshot);
                if !entry.coordination.is_null() {
                    state.insert("coordination".into(), entry.coordination);
                }
                Ok(Value::Object(state))
            }
            "resources.externalAdmission.reconcile" => {
                let Some(entry) = self.entries.get(key) else {
                    return Ok(json!(false));
                };
                Ok(json!(
                    entry.quarantined && entry.users == 0 && entry.waiters == 0 && !entry.releasing
                ))
            }
            "resources.externalAdmission.materialized" => {
                let Some(entry) = self.entries.get(key) else {
                    return Ok(json!(false));
                };
                let snapshot = &args["snapshot"];
                let context = snapshot["runtimeContextTokens"].as_f64().unwrap_or(0.0);
                let weights = entry.plan["unknownComponents"]
                    .as_array()
                    .is_some_and(|v| v.iter().any(|v| v == "kv-cache"))
                    && entry.plan["breakdown"]["weightIncrementBytes"]
                        .as_f64()
                        .unwrap_or(0.0)
                        > 0.0
                    && context > 0.0;
                let target = entry.plan["breakdown"]["targetContextTokens"]
                    .as_f64()
                    .unwrap_or(f64::INFINITY);
                Ok(json!(
                    snapshot["loaded"] == true && (weights || context >= target)
                ))
            }
            "resources.externalAdmission.close" => {
                self.closed = true;
                Ok(Value::Null)
            }
            _ => Err("AGENT_CORE_UNKNOWN_OPERATION".into()),
        }
    }
}

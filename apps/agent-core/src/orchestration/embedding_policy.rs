use serde_json::{Value, json};

const CPU_PROFILE: &str = "builtin-multilingual";
const GPU_PROFILE: &str = "builtin-multilingual-dml-q8";

fn supported(profile: &str) -> bool {
    [CPU_PROFILE, GPU_PROFILE].contains(&profile)
}
fn string(value: &Value) -> &str {
    value.as_str().unwrap_or("")
}

/// Exact vector-space identities remain mandatory while a background migration is incomplete.
/// 后台迁移尚未完成时仍须精确匹配向量空间身份，不能通过改标签假装兼容。
#[derive(Default)]
pub struct EmbeddingPolicyState {
    hardware: Option<Value>,
    hardware_at: f64,
    migrations: Vec<(String, Value)>,
}

impl EmbeddingPolicyState {
    pub fn apply(&mut self, operation: &str, args: &Value) -> Result<Value, String> {
        let local = &args["settings"]["local"];
        let configured = string(&local["embeddingProfileId"]);
        let policy = local["embeddingDevicePolicy"].as_str().unwrap_or("auto");
        match operation {
            "orchestration.embedding.target" => {
                if !supported(configured) || local["semantic"] == "off" {
                    return Ok(json!({"profileId":configured,"needsHardware":false}));
                }
                if policy == "cpu" {
                    return Ok(json!({"profileId":CPU_PROFILE,"needsHardware":false}));
                }
                if policy == "gpu" || configured == GPU_PROFILE {
                    return Ok(json!({"profileId":GPU_PROFILE,"needsHardware":false}));
                }
                if args.get("status").is_none() {
                    return Ok(
                        json!({"profileId":CPU_PROFILE,"needsHardware":false,"needsStatus":true}),
                    );
                }
                let status = &args["status"];
                let compatible = status["profileId"] == GPU_PROFILE
                    && status["embeddingSpaceId"] == args["gpuSpaceId"]
                    && status["state"]
                        .as_str()
                        .is_some_and(|state| ["ready", "loading"].contains(&state));
                Ok(
                    json!({"profileId":CPU_PROFILE,"needsHardware":compatible&&args["platform"]=="win32"}),
                )
            }
            "orchestration.embedding.hardwareNeeded" => Ok(json!(
                self.hardware.is_none()
                    || args["now"].as_f64().unwrap_or(0.0) - self.hardware_at > 2000.0
            )),
            "orchestration.embedding.hardware" => {
                if let Some(hardware) = args.get("hardware") {
                    self.hardware = Some(hardware.clone());
                    self.hardware_at = args["now"].as_f64().unwrap_or(0.0);
                }
                let hardware = self.hardware.as_ref().unwrap_or(&Value::Null);
                let gpu = hardware
                    .get("gpu")
                    .filter(|gpu| !gpu.is_null())
                    .unwrap_or(&hardware["hardware"]["gpu"]);
                Ok(json!(
                    gpu["state"] == "available"
                        && gpu["executionProvider"] == "dml"
                        && gpu["mappingStatus"] == "verified"
                ))
            }
            "orchestration.embedding.disabled" => Ok(
                json!({"profileId":configured,"targetProfileId":configured,"devicePreference":if policy=="cpu" {"cpu"} else {"auto"},"state":"disabled","fallbackProfileId":null,"canMigrate":false}),
            ),
            "orchestration.embedding.select" => {
                let target = string(&args["targetProfileId"]);
                let device = if policy == "cpu" { "cpu" } else { "auto" };
                if !supported(target) || args["hasVectorStatus"] != true {
                    return Ok(
                        json!({"profileId":target,"targetProfileId":target,"devicePreference":device,"state":"configured","fallbackProfileId":null}),
                    );
                }
                let corpora: Vec<_> = args["coverage"]["scopes"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .filter(|scope| scope["chunks"].as_f64().unwrap_or(0.0) > 0.0)
                    .collect();
                let matches = |space: &Value, id: &str| {
                    space["profileId"] == id && space["spaceId"] == args["spaceIds"][id]
                };
                let complete = |id: &str| {
                    !corpora.is_empty()
                        && corpora.iter().all(|scope| {
                            scope["spaces"].as_array().is_some_and(|spaces| {
                                spaces
                                    .iter()
                                    .any(|space| matches(space, id) && space["complete"] == true)
                            })
                        })
                };
                let present = |id: &str| {
                    corpora.iter().any(|scope| {
                        scope["spaces"].as_array().is_some_and(|spaces| {
                            spaces.iter().any(|space| {
                                matches(space, id) && space["vectors"].as_f64().unwrap_or(0.0) > 0.0
                            })
                        })
                    })
                };
                let cpu_fallback =
                    policy == "auto" && configured != GPU_PROFILE && present(CPU_PROFILE);
                let profile = if target == GPU_PROFILE && !complete(GPU_PROFILE) && cpu_fallback {
                    CPU_PROFILE
                } else {
                    target
                };
                Ok(
                    json!({"profileId":profile,"targetProfileId":target,"devicePreference":device,"state":if profile!=target {"migrating-with-compatible-cpu"} else if complete(target){"ready"}else if present(target){"partial"}else{"awaiting-index"},
                    "fallbackProfileId":if cpu_fallback&&profile==GPU_PROFILE{Some(CPU_PROFILE)}else{None},"coverage":args["coverage"],"canMigrate":!corpora.is_empty()&&!complete(target)&&!["unavailable","closed"].contains(&string(&args["targetStatus"]["state"])),"correctnessCertified":false}),
                )
            }
            "orchestration.embedding.schedule" => {
                let key = string(&args["key"]);
                let now = args["now"].as_f64().unwrap_or(0.0);
                if self.migrations.iter().any(|(k, entry)| {
                    k == key
                        && (entry["active"] == true
                            || entry["finishedAt"]
                                .as_f64()
                                .is_some_and(|finished| now - finished < 30000.0))
                }) {
                    return Ok(json!(false));
                }
                let entry = json!({"startedAt":now,"profileId":args["profileId"],"projectId":args["projectId"],"state":"preparing","active":true});
                if let Some((_, old)) = self.migrations.iter_mut().find(|(k, _)| k == key) {
                    *old = entry;
                } else {
                    self.migrations.push((key.into(), entry));
                }
                while self.migrations.len() > 32 {
                    let Some(index) = self
                        .migrations
                        .iter()
                        .position(|(_, entry)| entry["active"] != true)
                    else {
                        break;
                    };
                    self.migrations.remove(index);
                }
                Ok(json!(true))
            }
            "orchestration.embedding.finished" => {
                if let Some((_, entry)) = self
                    .migrations
                    .iter_mut()
                    .find(|(key, _)| key == string(&args["key"]))
                {
                    entry["active"] = json!(false);
                    entry["finishedAt"] = args["now"].clone();
                    if args.get("errorCode").is_some() {
                        entry["state"] = json!("failed");
                        entry["errorCode"] = args["errorCode"].clone();
                    } else {
                        entry["state"] = json!("background-job");
                        if let Some(id) = args.get("jobId") {
                            entry["jobId"] = id.clone();
                        }
                    }
                }
                Ok(Value::Null)
            }
            "orchestration.embedding.status" => {
                let migration = self
                    .migrations
                    .iter()
                    .map(|(_, entry)| {
                        let mut entry = entry.clone();
                        entry.as_object_mut().unwrap().remove("active");
                        if entry.get("jobId").is_some_and(|id| !id.is_null()) {
                            let job = args["jobs"]
                                .as_array()
                                .into_iter()
                                .flatten()
                                .find(|job| job["jobId"] == entry["jobId"]);
                            entry["jobState"] = job
                                .map(|job| job["status"].clone())
                                .unwrap_or(json!("not-observed"));
                            entry["indexedCoverageNeedsVerification"] = json!(true);
                        }
                        entry
                    })
                    .collect::<Vec<_>>();
                Ok(
                    json!({"platform":"windows","supportedProfiles":[CPU_PROFILE,GPU_PROFILE],"retainedSpaces":2,"migration":migration,"automaticWeightRelabeling":false}),
                )
            }
            _ => Err("AGENT_CORE_UNKNOWN_OPERATION".into()),
        }
    }
}

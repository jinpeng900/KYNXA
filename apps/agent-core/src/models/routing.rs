use std::collections::{BTreeMap, HashMap};

use serde_json::{Value, json};

#[derive(Clone)]
struct Owner {
    owner_id: u64,
    profile_id: String,
    preference: String,
}
impl Owner {
    fn value(&self) -> Value {
        json!({"ownerId": self.owner_id, "profileId": self.profile_id, "preference": self.preference})
    }
}

/// Profile/device routing and failed-retirement barriers are owned by Rust, independent of JS objects.
/// Rust 拥有配置/设备路由及退役失败屏障；JS 只保留实例对象和对应的异步完成句柄。
#[derive(Default)]
pub struct RoutingState {
    kind: String,
    closed: bool,
    sequence: u64,
    preferences: HashMap<String, String>,
    active: BTreeMap<String, Owner>,
    retiring: BTreeMap<String, Owner>,
    idle_diagnostic: Value,
}

impl RoutingState {
    fn prefix(&self) -> &str {
        if self.kind == "embedding" {
            "EMBEDDING"
        } else {
            "RERANK"
        }
    }
    fn validate_preference(&self, args: &Value) -> Result<(), String> {
        let preference = match args.get("devicePreference") {
            Some(value) => value
                .as_str()
                .ok_or("INFERENCE_INVALID_DEVICE_PREFERENCE")?,
            None => "auto",
        };
        if !["auto", "cpu"].contains(&preference) {
            return Err("INFERENCE_INVALID_DEVICE_PREFERENCE".into());
        }
        if args["requiredDevice"].as_str().is_some() && preference == "cpu" {
            return Err(format!("{}_GPU_REQUIRED", self.prefix()));
        }
        Ok(())
    }
    fn retire_owner(&mut self, profile_id: &str) -> Option<Value> {
        let owner = self.active.remove(profile_id)?;
        let value = owner.value();
        self.retiring.insert(profile_id.into(), owner);
        Some(value)
    }
    pub fn apply(&mut self, operation: &str, args: &Value) -> Result<Value, String> {
        let profile_id = args["profileId"].as_str().unwrap_or("");
        match operation {
            "models.routing.init" => {
                self.kind = args["kind"]
                    .as_str()
                    .ok_or("RETRIEVAL_INVALID_MODEL_KIND")?
                    .into();
                if !["embedding", "reranker"].contains(&self.kind.as_str()) {
                    return Err("RETRIEVAL_INVALID_MODEL_KIND".into());
                }
                let idle = args["idleReleaseMs"]
                    .as_f64()
                    .filter(|value| value.is_finite() && *value > 0.0)
                    .unwrap_or(120000.0);
                Ok(json!({"idleReleaseMs": idle, "timerIntervalMs": idle.clamp(25.0, 30000.0)}))
            }
            "models.routing.idleDiagnostic" => {
                if args["set"] == true {
                    self.idle_diagnostic = args["value"].clone();
                }
                Ok(self.idle_diagnostic.clone())
            }
            "models.routing.idleSummary" => {
                let results: Vec<Value> = args["results"]
                    .as_array()
                    .map(|results| {
                        results
                            .iter()
                            .filter(|result| !result.is_null() && **result != false)
                            .cloned()
                            .collect()
                    })
                    .unwrap_or_default();
                Ok(
                    json!({"released": results.iter().any(|result| result["released"] == true), "results": results}),
                )
            }
            "models.routing.view" => {
                let owner = self
                    .retiring
                    .get(profile_id)
                    .or_else(|| self.active.get(profile_id));
                let preference = self
                    .retiring
                    .get(profile_id)
                    .map(|owner| owner.preference.as_str())
                    .or_else(|| self.preferences.get(profile_id).map(String::as_str))
                    .or_else(|| args["devicePreference"].as_str())
                    .unwrap_or("auto");
                let mut value = json!({"closed": self.closed, "preference": preference, "owner": owner.map(Owner::value),
                    "devicePreference": preference, "executionIdentity": format!("{profile_id}:{preference}"),
                    "retiring": self.retiring.contains_key(profile_id)});
                if let Some(requested) = args["devicePreference"]
                    .as_str()
                    .filter(|requested| *requested != preference)
                {
                    value["requestedDevicePreference"] = json!(requested);
                    value["requiresReconfiguration"] = json!(true);
                }
                Ok(value)
            }
            "models.routing.ensure" => {
                if self.closed {
                    return Err(format!("{}_CLOSED", self.prefix()));
                }
                if self.retiring.contains_key(profile_id) {
                    return Err(format!("{}_RETIREMENT_PENDING", self.prefix()));
                }
                if let Some(owner) = self.active.get(profile_id) {
                    return Ok(json!({"created": false, "owner": owner.value()}));
                }
                let preference = args["devicePreference"]
                    .as_str()
                    .or_else(|| self.preferences.get(profile_id).map(String::as_str))
                    .unwrap_or("auto");
                self.sequence = self
                    .sequence
                    .checked_add(1)
                    .ok_or("INFERENCE_ROUTER_LIMIT")?;
                let owner = Owner {
                    owner_id: self.sequence,
                    profile_id: profile_id.into(),
                    preference: preference.into(),
                };
                let value = owner.value();
                self.preferences
                    .insert(profile_id.into(), preference.into());
                self.active.insert(profile_id.into(), owner);
                Ok(json!({"created": true, "owner": value}))
            }
            "models.routing.creationFailed" => {
                if self
                    .active
                    .get(profile_id)
                    .is_some_and(|owner| Some(owner.owner_id) == args["ownerId"].as_u64())
                {
                    self.active.remove(profile_id);
                }
                Ok(Value::Null)
            }
            "models.routing.validate" => {
                self.validate_preference(args)?;
                Ok(Value::Null)
            }
            "models.routing.configure" => {
                if self.closed {
                    return Err(format!("{}_CLOSED", self.prefix()));
                }
                let preference = args["devicePreference"].as_str().unwrap_or("auto");
                self.validate_preference(args)?;
                let retire_others = args["retireOtherProfiles"] == true;
                let ids: Vec<String> = self
                    .active
                    .iter()
                    .filter(|(id, owner)| {
                        if id.as_str() == profile_id {
                            owner.preference != preference
                        } else {
                            retire_others
                        }
                    })
                    .map(|(id, _)| id.clone())
                    .collect();
                let retire: Vec<Value> =
                    ids.iter().filter_map(|id| self.retire_owner(id)).collect();
                self.preferences
                    .insert(profile_id.into(), preference.into());
                let barriers: Vec<Value> = self
                    .retiring
                    .iter()
                    .filter(|(id, _)| id.as_str() == profile_id || retire_others)
                    .map(|(_, owner)| owner.value())
                    .collect();
                Ok(
                    json!({"retire": retire, "barriers": barriers, "profileId": profile_id, "devicePreference": preference,
                    "executionIdentity": format!("{profile_id}:{preference}")}),
                )
            }
            "models.routing.retire" => {
                let ids: Vec<String> = self
                    .active
                    .keys()
                    .filter(|id| profile_id.is_empty() || id.as_str() == profile_id)
                    .cloned()
                    .collect();
                let retire: Vec<Value> =
                    ids.iter().filter_map(|id| self.retire_owner(id)).collect();
                let barriers: Vec<Value> = self
                    .retiring
                    .iter()
                    .filter(|(id, _)| profile_id.is_empty() || id.as_str() == profile_id)
                    .map(|(_, owner)| owner.value())
                    .collect();
                Ok(
                    json!({"retire": retire, "barriers": barriers, "retired": true,
                    "profileId": if profile_id.is_empty() { Value::Null } else { json!(profile_id) }}),
                )
            }
            "models.routing.claimIdle" => {
                let owner_id = args["ownerId"].as_u64();
                if self
                    .active
                    .get(profile_id)
                    .is_some_and(|owner| Some(owner.owner_id) == owner_id)
                {
                    Ok(self.retire_owner(profile_id).unwrap())
                } else {
                    Err("INFERENCE_ROUTER_OWNER_CHANGED".into())
                }
            }
            "models.routing.retired" => {
                if self
                    .retiring
                    .get(profile_id)
                    .is_some_and(|owner| Some(owner.owner_id) == args["ownerId"].as_u64())
                {
                    self.retiring.remove(profile_id);
                }
                Ok(Value::Null)
            }
            "models.routing.owners" => Ok(
                json!({"active": self.active.values().map(Owner::value).collect::<Vec<_>>(),
                "retiring": self.retiring.values().map(Owner::value).collect::<Vec<_>>(), "closed": self.closed}),
            ),
            "models.routing.close" => {
                self.closed = true;
                Ok(Value::Null)
            }
            _ => Err("AGENT_CORE_UNKNOWN_OPERATION".into()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn preference_switch_keeps_failed_retirement_barrier_and_closed_status_cannot_create() {
        let mut routing = RoutingState::default();
        routing
            .apply("models.routing.init", &json!({"kind": "embedding"}))
            .unwrap();
        let owner = routing
            .apply("models.routing.ensure", &json!({"profileId": "profile"}))
            .unwrap()["owner"]
            .clone();
        let plan = routing
            .apply(
                "models.routing.configure",
                &json!({"profileId": "profile", "devicePreference": "cpu"}),
            )
            .unwrap();
        assert_eq!(plan["barriers"][0], owner);
        assert_eq!(
            routing.apply("models.routing.ensure", &json!({"profileId": "profile"})),
            Err("EMBEDDING_RETIREMENT_PENDING".into())
        );
        routing
            .apply(
                "models.routing.retired",
                &json!({"profileId": "profile", "ownerId": owner["ownerId"]}),
            )
            .unwrap();
        assert_eq!(
            routing
                .apply("models.routing.view", &json!({"profileId": "profile"}))
                .unwrap()["preference"],
            "cpu"
        );
        routing.apply("models.routing.close", &json!({})).unwrap();
        assert_eq!(
            routing.apply("models.routing.ensure", &json!({"profileId": "profile"})),
            Err("EMBEDDING_CLOSED".into())
        );
    }
}

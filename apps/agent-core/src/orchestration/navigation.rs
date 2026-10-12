use serde_json::{Value, json};

fn valid_text(value: &Value, maximum: usize) -> bool {
    value.as_str().is_some_and(|text| {
        !text.trim().is_empty() && text.encode_utf16().count() <= maximum && !text.contains('\0')
    })
}

/// The model's plan is revisable state, never a permission or completion certificate.
/// 模型计划是可修正状态，不授予权限，也不认证任务完成。
#[derive(Default)]
pub struct NavigationState {
    revision: u64,
    original_characters: u64,
    interpretation: Vec<(String, String)>,
    interpreted: bool,
    candidates: Vec<(String, String)>,
}

impl NavigationState {
    fn validate(&self, input: &Value) -> Result<(), String> {
        let object = input.as_object().ok_or("INVALID_RETRIEVAL_INPUT")?;
        if object.keys().any(|key| {
            ![
                "expectedRevision",
                "meaning",
                "taskRelation",
                "channel",
                "gaps",
                "stop",
                "candidates",
            ]
            .contains(&key.as_str())
        }) {
            return Err("INVALID_RETRIEVAL_INPUT".into());
        }
        if object
            .get("expectedRevision")
            .is_some_and(|value| value.as_u64() != Some(self.revision))
        {
            return Err("EVIDENCE_PLAN_CONFLICT".into());
        }
        if object.get("meaning").is_some_and(|v| !valid_text(v, 2000)) {
            return Err("INVALID_RETRIEVAL_INPUT".into());
        }
        for (field, allowed) in [
            (
                "taskRelation",
                &["new", "continue", "supplement", "correction"][..],
            ),
            (
                "channel",
                &["none", "local", "source-read", "relations", "web", "tools"][..],
            ),
            (
                "stop",
                &["continue", "answer-supported", "insufficient"][..],
            ),
        ] {
            if object
                .get(field)
                .is_some_and(|v| !v.as_str().is_some_and(|v| allowed.contains(&v)))
            {
                return Err("INVALID_RETRIEVAL_INPUT".into());
            }
        }
        if let Some(gaps) = object.get("gaps") {
            let gaps = gaps.as_array().ok_or("INVALID_RETRIEVAL_INPUT")?;
            if gaps.len() > 32 || gaps.iter().any(|v| !valid_text(v, 1000)) {
                return Err("INVALID_RETRIEVAL_INPUT".into());
            }
        }
        if let Some(candidates) = object.get("candidates") {
            let candidates = candidates.as_array().ok_or("INVALID_RETRIEVAL_INPUT")?;
            if candidates.len() > 32
                || candidates.iter().any(|v| {
                    !valid_text(&v["sourceRef"], 4096)
                        || !valid_text(&v["reason"], 1000)
                        || !v["decision"]
                            .as_str()
                            .is_some_and(|v| ["investigate", "defer", "reject"].contains(&v))
                })
            {
                return Err("INVALID_RETRIEVAL_INPUT".into());
            }
        }
        Ok(())
    }

    pub fn apply(&mut self, operation: &str, args: &Value) -> Result<Value, String> {
        match operation {
            "orchestration.navigation.open" => {
                self.original_characters = args["originalCharacters"].as_u64().unwrap_or(0);
                Ok(Value::Null)
            }
            "orchestration.navigation.validate" => {
                if args["inputKeys"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .any(|key| {
                        !key.as_str().is_some_and(|key| {
                            [
                                "expectedRevision",
                                "meaning",
                                "taskRelation",
                                "channel",
                                "gaps",
                                "stop",
                                "candidates",
                            ]
                            .contains(&key)
                        })
                    })
                {
                    return Err("INVALID_RETRIEVAL_INPUT".into());
                }
                self.validate(&args["input"])?;
                Ok(Value::Null)
            }
            "orchestration.navigation.update" => {
                if args["inputKeys"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .any(|key| {
                        !key.as_str().is_some_and(|key| {
                            [
                                "expectedRevision",
                                "meaning",
                                "taskRelation",
                                "channel",
                                "gaps",
                                "stop",
                                "candidates",
                            ]
                            .contains(&key)
                        })
                    })
                {
                    return Err("INVALID_RETRIEVAL_INPUT".into());
                }
                let input = &args["input"];
                self.validate(input)?;
                if input
                    .as_object()
                    .unwrap()
                    .keys()
                    .any(|key| key != "expectedRevision")
                {
                    if input["taskRelation"] == "new" {
                        self.candidates.clear();
                        self.interpretation.clear();
                    }
                    for field in ["meaning", "taskRelation", "channel", "gaps", "stop"] {
                        if input.get(field).is_none() {
                            continue;
                        }
                        let raw = args["rawFields"][field]
                            .as_str()
                            .ok_or("INVALID_RETRIEVAL_INPUT")?
                            .to_owned();
                        if let Some((_, previous)) =
                            self.interpretation.iter_mut().find(|(key, _)| key == field)
                        {
                            *previous = raw;
                        } else {
                            self.interpretation.push((field.into(), raw));
                        }
                    }
                    self.interpreted = true;
                    for candidate in args["verifiedCandidates"].as_array().unwrap_or(&Vec::new()) {
                        let key = candidate["sourceRef"]
                            .as_str()
                            .ok_or("INVALID_RETRIEVAL_INPUT")?
                            .to_owned();
                        let raw = candidate["raw"]
                            .as_str()
                            .ok_or("INVALID_RETRIEVAL_INPUT")?
                            .to_owned();
                        if let Some((_, previous)) =
                            self.candidates.iter_mut().find(|(k, _)| k == &key)
                        {
                            *previous = raw;
                        } else {
                            self.candidates.push((key, raw));
                        }
                    }
                    let omitted = self.candidates.len().saturating_sub(128);
                    self.candidates.drain(..omitted);
                    self.revision += 1;
                }
                self.apply("orchestration.navigation.snapshot", &json!({}))
            }
            "orchestration.navigation.snapshot" => {
                let offset = args
                    .get("offset")
                    .unwrap_or(&json!(0))
                    .as_u64()
                    .filter(|v| *v <= 128)
                    .ok_or("INVALID_RETRIEVAL_INPUT")? as usize;
                let limit = args
                    .get("limit")
                    .unwrap_or(&json!(32))
                    .as_u64()
                    .filter(|v| *v > 0 && *v <= 32)
                    .ok_or("INVALID_RETRIEVAL_INPUT")? as usize;
                let total = self.candidates.len();
                let interpretation = if self.interpreted {
                    Some(format!(
                        "{{{},\"author\":\"model\",\"correctnessCertified\":false}}",
                        self.interpretation
                            .iter()
                            .map(|(key, raw)| format!("\"{key}\":{raw}"))
                            .collect::<Vec<_>>()
                            .join(",")
                    ))
                } else {
                    None
                };
                Ok(
                    json!({"schemaVersion":1,"revision":self.revision,"originalRequestVisible":self.original_characters<=8192,"originalRequestReference":"current-user-message","originalRequestCharacters":self.original_characters,
                    "interpretationRaw":interpretation,"candidateRecords":self.candidates.iter().skip(offset).take(limit).map(|(_,raw)|raw).collect::<Vec<_>>(),"offset":offset,"total":total,"nextOffset":total.min(offset+limit),"hasMore":offset+limit<total,
                    "grantsPermission":false,"filtersRetrieval":false,"executesActions":false,"notice":"This is a revisable model plan, not verified intent or task completion. Execute the chosen tool separately; source validation proves identity/version only. Re-read candidates before assessing support."}),
                )
            }
            "orchestration.navigation.export" => Ok(
                json!({"revision":self.revision,"originalCharacters":self.original_characters,"interpretation":self.interpretation,"interpreted":self.interpreted,"candidates":self.candidates}),
            ),
            "orchestration.navigation.import" => {
                self.revision = args["revision"].as_u64().ok_or("INVALID_RETRIEVAL_INPUT")?;
                self.original_characters = args["originalCharacters"]
                    .as_u64()
                    .ok_or("INVALID_RETRIEVAL_INPUT")?;
                self.interpretation = serde_json::from_value(args["interpretation"].clone())
                    .map_err(|_| "INVALID_RETRIEVAL_INPUT")?;
                self.candidates = serde_json::from_value(args["candidates"].clone())
                    .map_err(|_| "INVALID_RETRIEVAL_INPUT")?;
                self.interpreted = args["interpreted"] == true;
                Ok(Value::Null)
            }
            _ => Err("AGENT_CORE_UNKNOWN_OPERATION".into()),
        }
    }
}

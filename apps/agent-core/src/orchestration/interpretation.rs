use serde_json::{Value, json};

fn present(value: &Value) -> bool {
    !value.is_null() && value != false && value != "" && value != 0
}
fn outcome(args: &Value) -> Value {
    let result = &args["result"];
    if !args["error"].is_null() {
        let raw = args["error"]["code"].as_str().unwrap_or("");
        let code = if regex::Regex::new(r"^[A-Z][A-Z0-9_]{0,79}$")
            .unwrap()
            .is_match(raw)
        {
            raw
        } else {
            "RETRIEVAL_FAILED"
        };
        let matches = |pattern: &str| regex::Regex::new(pattern).unwrap().is_match(code);
        let (state, next) =
            if matches("STALE|SOURCE_CHANGED|REFERENCE.*(?:STALE|INVALID)|INVALID.*REFERENCE") {
                ("source-changed", "resolve-or-read-current-source")
            } else if matches("INVALID|ARGUMENT|SCOPE_MISMATCH") {
                (
                    "invalid-arguments",
                    "correct-parameters-with-current-evidence",
                )
            } else if matches("UNAVAILABLE|DISABLED|CLOSED|UNSUPPORTED") {
                (
                    "tool-unavailable",
                    "discover-available-authorized-alternative",
                )
            } else {
                ("execution-failed", "inspect-failure-before-retrying")
            };
        return json!({"state":state,"code":code,"next":next});
    }
    let partial = result["indexingPending"] == true
        || result["indexingPartial"] == true
        || result["sourceCoverage"]["complete"] == false;
    if ["items", "references"].iter().any(|name| {
        result[name]
            .as_array()
            .is_some_and(|items| !items.is_empty())
    }) {
        return json!({"state":"evidence-found","next":"read-and-check-support","correctnessCertified":false,"coverage":if partial{"partial"}else{"bounded"}});
    }
    let (state, next, certified) = if present(&result["selection"]["staleSourceCount"])
        || present(&result["selection"]["finalStaleSourceCount"])
    {
        ("source-changed", "resolve-or-read-current-source", false)
    } else if result["strategy"] == "disabled" {
        (
            "tool-unavailable",
            "discover-available-authorized-alternative",
            true,
        )
    } else if result["strategy"] == "context-budget-exhausted"
        || present(&result["selection"]["omittedForBudget"])
        || present(&result["selection"]["projectionOmittedCount"])
    {
        (
            "evidence-budget-exhausted",
            "use-current-evidence-or-state-context-limit",
            false,
        )
    } else if present(&result["selection"]["alreadyPresentCount"])
        || result["evidenceAssessment"]["reason"] == "already-in-context"
    {
        (
            "evidence-already-in-context",
            "check-existing-context-support",
            false,
        )
    } else if partial {
        (
            "coverage-incomplete",
            "read-known-source-or-check-index-progress",
            false,
        )
    } else {
        (
            "no-match-in-current-coverage",
            "inspect-scope-and-evidence-gap",
            false,
        )
    };
    let mut value = json!({"state":state,"next":next});
    if !certified {
        value["correctnessCertified"] = json!(false);
    }
    value
}

/// Projection preserves the user's original request; derived hints never grant authority.
/// 投影保留用户原始请求；派生线索不授予任何权限。
pub fn calculate(operation: &str, args: &Value) -> Result<Value, String> {
    match operation {
        "orchestration.retrievalOutcome" => Ok(outcome(args)),
        "orchestration.requestPolicy" => {
            let message = args["message"].as_str().unwrap_or("");
            let matches = |pattern: &str| regex::Regex::new(pattern).unwrap().is_match(message);
            let local = args["referenceCount"].as_u64().unwrap_or(0) > 0
                && matches("(?i)根据资料|本地|记忆|之前|上次|工作文件|项目文件")
                && !matches(
                    "(?i)最新|联网|网上|网页|搜索|浏览器|https?:|latest|online|web|browser",
                );
            let budget = args["inputBudgetTokens"].as_f64().unwrap_or(0.0);
            let ceiling = args["schemaCeilingTokens"].as_f64().unwrap_or(0.0);
            let essential = args["essentialTokens"].as_f64().unwrap_or(0.0);
            Ok(
                json!({"connectMcp":if local {json!(false)}else{json!("relevant")},
                "schemaCeilingTokens":if args["deviceState"]==true {ceiling.max((essential+64.0).min((budget*0.65).floor()))}else{ceiling}}),
            )
        }
        "orchestration.interpretation" => {
            let plan = &args["plan"];
            let relation = plan
                .get("taskRelation")
                .filter(|v| !v.is_null())
                .cloned()
                .unwrap_or(json!({"type":"uncertain","allowsInheritance":false}));
            let mut task = json!({"allowsInheritance":relation["allowsInheritance"]==true});
            if let Some(v) = relation.get("type") {
                task["type"] = v.clone();
            }
            if present(&relation["reason"]) {
                task["reason"] = relation["reason"].clone();
            }
            let mut retrieval = json!({"automatic":plan["shouldRetrieve"]==true,"domain":plan.get("domain").filter(|v|!v.is_null()).unwrap_or(&json!("mixed")),
                "domainConstraint":plan.get("domainConstraint").filter(|v|!v.is_null()).unwrap_or(&json!("none")),"operation":plan.get("operation").filter(|v|!v.is_null()).unwrap_or(&json!("search"))});
            if present(&plan["preferredDomain"]) {
                retrieval["preferredDomain"] = plan["preferredDomain"].clone();
            }
            let clues = plan["clues"]
                .as_array()
                .map(|a| {
                    a.iter()
                        .take(12)
                        .map(|clue| {
                            let mut projected = json!({});
                            for key in ["kind", "value", "strength", "origin", "verified"] {
                                if let Some(v) = clue.get(key) {
                                    projected[key] = v.clone();
                                }
                            }
                            if present(&clue["basis"]) {
                                projected["basis"] = clue["basis"].clone();
                            }
                            if clue["startOffset"].is_i64() || clue["startOffset"].is_u64() {
                                projected["startOffset"] = clue["startOffset"].clone();
                                if let Some(v) = clue.get("endOffset") {
                                    projected["endOffset"] = v.clone();
                                }
                            }
                            projected
                        })
                        .collect::<Vec<_>>()
                })
                .unwrap_or_default();
            let derivation = plan
                .get("queryDerivation")
                .filter(|v| !v.is_null())
                .cloned()
                .unwrap_or_else(|| {
                    let mut value = json!({"additions":[],"preservedOriginal":true});
                    if let Some(v) = plan.get("originalQuery") {
                        value["originalQuery"] = v.clone();
                    }
                    value
                });
            Ok(
                json!({"schemaVersion":1,"taskRelation":task,"retrieval":retrieval,"clues":clues,"queryDerivation":derivation,"grantsPermission":false,"interpretationVerified":false}),
            )
        }
        "orchestration.interpretationHints" => {
            let value = &args["interpretation"];
            let relation = &value["taskRelation"];
            let weak = value["clues"]
                .as_array()
                .into_iter()
                .flatten()
                .filter(|c| c["strength"] == "weak")
                .collect::<Vec<_>>();
            let additions = value["queryDerivation"]["additions"]
                .as_array()
                .cloned()
                .unwrap_or_default();
            let replacements = value["queryDerivation"]["replacements"]
                .as_array()
                .map(Vec::len)
                .unwrap_or(0);
            let preferred = &value["retrieval"]["preferredDomain"];
            if relation["type"] == "new"
                && weak.is_empty()
                && additions.is_empty()
                && replacements == 0
                && !present(preferred)
            {
                return Ok(Value::Null);
            }
            let mut hints =
                json!({"relation":relation["type"],"inherit":relation["allowsInheritance"]});
            if present(preferred) {
                hints["preferredDomain"] = preferred.clone();
            }
            if !weak.is_empty() {
                let mut kinds = Vec::new();
                for clue in weak {
                    if !kinds.contains(&clue["kind"]) {
                        kinds.push(clue["kind"].clone());
                    }
                    if kinds.len() == 3 {
                        break;
                    }
                }
                hints["candidateKinds"] = json!(kinds);
            }
            if !additions.is_empty() {
                hints["additions"] = json!(
                    additions
                        .iter()
                        .take(2)
                        .map(|a| {
                            let mut record = json!({});
                            for k in ["origin", "basis"] {
                                if let Some(v) = a.get(k) {
                                    record[k] = v.clone();
                                }
                            }
                            if present(&a["historyId"]) {
                                record["historyId"] = a["historyId"].clone();
                            }
                            record
                        })
                        .collect::<Vec<_>>()
                );
            }
            if replacements > 0 {
                hints["explicitCorrectionCount"] = json!(replacements);
            }
            Ok(hints)
        }
        _ => Err("AGENT_CORE_UNKNOWN_OPERATION".into()),
    }
}

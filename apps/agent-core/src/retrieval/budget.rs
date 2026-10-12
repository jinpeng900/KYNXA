use serde_json::{Value, json};

fn bounded(value: &Value, fallback: i64, minimum: i64, maximum: i64) -> i64 {
    value
        .as_i64()
        .filter(|number| number.unsigned_abs() <= 9_007_199_254_740_991)
        .map_or(fallback, |number| number.clamp(minimum, maximum))
}

fn rerank_limit(value: &Value, fallback: i64) -> i64 {
    value
        .as_i64()
        .filter(|value| [20, 40, 60].contains(value))
        .unwrap_or(fallback)
}

/// Record configuration, grants and truncation together; candidate capacity is independent from final references.
/// 同时记录配置、授额与截断；内部候选容量独立于最终引用额度，资源建议不能改变用户上限。
pub fn apply(args: &Value) -> Result<Value, String> {
    if let Some(budget) = args.get("budget").filter(|value| value.is_object()) {
        return finalize_rerank_budget(budget.clone(), args);
    }
    let task = args["taskType"].as_str().unwrap_or("lookup");
    let complex = ["complex", "research"].contains(&task);
    let suggestions = &args["suggestions"];
    let configured = json!({"channelCandidates": if complex {128} else {64},
        "fusedCandidates":if complex {160} else {96},"limit":if complex {40} else if task=="file" {24} else {16},
        "maximumTokens":if complex {32768} else if task=="file" {16384} else {8192},
        "rerankCandidates":if complex {60} else if task=="file" {40} else {20}});
    let tokens = args
        .get("maximumTokens")
        .filter(|value| !value.is_null())
        .cloned()
        .unwrap_or_else(|| {
            json!(bounded(
                &suggestions["evidenceBudgetTokens"],
                configured["maximumTokens"].as_i64().unwrap_or(8192),
                0,
                32768
            ))
        });
    let mut budget = json!({"channelCandidates":bounded(&suggestions["candidateLimit"],if complex {128} else {64},1,if complex {160} else {96}),
        "fusedCandidates":bounded(&suggestions["fusedCandidateLimit"],if complex {160} else {96},1,if complex {192} else {128}),
        "limit":args.get("limit").filter(|value| !value.is_null()).unwrap_or(&configured["limit"]),"maximumTokens":tokens,
        "rerankCandidates":rerank_limit(&suggestions["rerankCandidates"],configured["rerankCandidates"].as_i64().unwrap_or(20)).min(rerank_limit(&suggestions["rerankCandidateLimit"],60)),
        "source":suggestions.get("source").filter(|value| !value.is_null()).cloned().unwrap_or(json!("task-policy"))});
    let mut cuts = Vec::new();
    for (suggestion, approved, unit) in [
        ("candidateLimit", "channelCandidates", "candidates"),
        ("fusedCandidateLimit", "fusedCandidates", "candidates"),
        ("evidenceBudgetTokens", "maximumTokens", "tokens"),
    ] {
        let proposed = suggestions[suggestion]
            .as_i64()
            .filter(|number| number.unsigned_abs() <= 9_007_199_254_740_991);
        let granted = budget[approved].as_i64();
        if let (Some(proposed), Some(granted)) = (proposed, granted) {
            let requested_tokens =
                args.get("maximumTokens").is_some() && approved == "maximumTokens";
            if proposed > granted {
                cuts.push(json!({"reason":if requested_tokens {"requested-evidence-token-limit"} else {"candidate-or-evidence-policy-limit"},"field":approved,"proposed":proposed,"approved":granted,"unit":unit}));
            }
            if granted < configured[approved].as_i64().unwrap_or(0) && !requested_tokens {
                cuts.push(json!({"reason":"resource-grant","field":approved,"proposed":configured[approved],"approved":granted,"unit":unit}));
            }
        }
    }
    let resource_suggestions: serde_json::Map<String, Value> = [
        "candidateLimit",
        "fusedCandidateLimit",
        "evidenceBudgetTokens",
        "rerankCandidates",
        "rerankCandidateLimit",
    ]
    .iter()
    .filter_map(|key| {
        suggestions
            .get(*key)
            .map(|value| ((*key).to_owned(), value.clone()))
    })
    .collect();
    budget["audit"] = json!({"requested":{"limit":args["limit"],"maximumTokens":args["maximumTokens"]},"configured":configured,"resourceSuggestions":resource_suggestions,
        "approved":{"channelCandidates":budget["channelCandidates"],"fusedCandidates":budget["fusedCandidates"],"limit":budget["limit"],"maximumTokens":budget["maximumTokens"],"rerankCandidates":budget["rerankCandidates"]},
        "downstreamLimits":{"channelCandidates":160,"fusedCandidates":192,"finalReferences":60,"maximumTokens":32768},"actual":null,"earlyCutReasons":cuts});
    Ok(budget)
}

/// Local settings override adaptive requests, while the actual resource grant remains authoritative.
/// 本地设置优先于自适应请求，但实际资源授额仍是硬上限；保留原候选审计与其他预算。
fn finalize_rerank_budget(mut budget: Value, args: &Value) -> Result<Value, String> {
    let configured = args
        .get("localRerankCandidates")
        .filter(|value| !value.is_null());
    let requested = configured
        .unwrap_or(&budget["rerankCandidates"])
        .as_i64()
        .ok_or_else(|| "AGENT_CORE_INVALID_ARGUMENT".to_owned())?;
    let resource_limit = rerank_limit(&args["suggestions"]["rerankCandidateLimit"], 60);
    let approved = requested.min(resource_limit);
    budget["rerankCandidates"] = json!(approved);
    let audit = &mut budget["audit"];
    audit["configured"]["localRerankCandidates"] = configured.cloned().unwrap_or(Value::Null);
    audit["approved"]["rerankCandidates"] = json!(approved);
    audit["rerankRequestedCandidates"] = json!(requested);
    audit["resourceRerankLimit"] = json!(resource_limit);
    audit["rerankBudgetSource"] = json!(if configured.is_some() {
        "local-setting"
    } else if args["suggestions"].get("rerankCandidates").is_some() {
        "resource-suggestion"
    } else {
        "task-policy"
    });
    if approved < requested {
        audit["earlyCutReasons"]
            .as_array_mut()
            .ok_or_else(|| "AGENT_CORE_INVALID_ARGUMENT".to_owned())?
            .push(json!({"reason":"resource-grant","field":"rerankCandidates",
                "proposed":requested,"approved":approved,"unit":"candidates"}));
    }
    Ok(budget)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn local_rerank_request_preserves_priority_and_actual_grant_audit() {
        let suggestions =
            json!({"rerankCandidates":60,"rerankCandidateLimit":20,"candidateLimit":1});
        let original = apply(&json!({"taskType":"research","suggestions":suggestions})).unwrap();
        let result =
            apply(&json!({"budget":original,"localRerankCandidates":40,"suggestions":suggestions}))
                .unwrap();
        assert_eq!(result["rerankCandidates"], 20);
        assert_eq!(result["audit"]["configured"]["localRerankCandidates"], 40);
        assert_eq!(result["audit"]["rerankRequestedCandidates"], 40);
        assert_eq!(result["audit"]["rerankBudgetSource"], "local-setting");
        let original_cuts = original["audit"]["earlyCutReasons"].as_array().unwrap();
        let result_cuts = result["audit"]["earlyCutReasons"].as_array().unwrap();
        assert_eq!(&result_cuts[..original_cuts.len()], original_cuts);
        assert_eq!(
            result_cuts.last().unwrap(),
            &json!({"reason":"resource-grant","field":"rerankCandidates",
            "proposed":40,"approved":20,"unit":"candidates"})
        );
    }

    #[test]
    fn adaptive_rerank_and_task_defaults_keep_source_and_invalid_grant_behavior() {
        for (suggestions, expected, source) in [
            (
                json!({"rerankCandidates":60,"rerankCandidateLimit":99}),
                60,
                "resource-suggestion",
            ),
            (
                json!({"rerankCandidates":60,"rerankCandidateLimit":20}),
                20,
                "resource-suggestion",
            ),
            (json!({}), 40, "task-policy"),
        ] {
            let budget = apply(&json!({"taskType":"file","suggestions":suggestions})).unwrap();
            let result = apply(
                &json!({"budget":budget,"localRerankCandidates":null,"suggestions":suggestions}),
            )
            .unwrap();
            assert_eq!(result["rerankCandidates"], expected);
            assert_eq!(result["audit"]["rerankBudgetSource"], source);
            assert_eq!(
                result["audit"]["earlyCutReasons"],
                budget["audit"]["earlyCutReasons"]
            );
        }
    }
}

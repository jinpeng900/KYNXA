use serde_json::{Value, json};

fn bounded(value: &Value, fallback: i64, minimum: i64, maximum: i64) -> i64 {
    value
        .as_i64()
        .filter(|number| number.unsigned_abs() <= 9_007_199_254_740_991)
        .map_or(fallback, |number| number.clamp(minimum, maximum))
}

/// Record configuration, grants and truncation together; candidate capacity is independent from final references.
/// 同时记录配置、授额与截断；内部候选容量独立于最终引用额度，资源建议不能改变用户上限。
pub fn apply(args: &Value) -> Result<Value, String> {
    if let Some(budget) = args.get("budget").filter(|value| value.is_object()) {
        return Ok(retire_rerank_budget(budget.clone()));
    }
    let task = args["taskType"].as_str().unwrap_or("lookup");
    let complex = ["complex", "research"].contains(&task);
    let suggestions = &args["suggestions"];
    let configured = json!({"channelCandidates": if complex {128} else {64},
        "fusedCandidates":if complex {160} else {96},"limit":if complex {40} else if task=="file" {24} else {16},
        "maximumTokens":if complex {32768} else if task=="file" {16384} else {8192},
        "rerankCandidates":null});
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
        "rerankCandidates":null,
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
    Ok(retire_rerank_budget(budget))
}

/// Keep retired fields readable for one compatibility version without reserving a neural-rerank budget.
/// 旧字段保留一个兼容版本并统一为 null，不再计算或申请神经重排额度。
fn retire_rerank_budget(mut budget: Value) -> Value {
    budget["rerankCandidates"] = Value::Null;
    budget["audit"]["configured"]["rerankCandidates"] = Value::Null;
    budget["audit"]["configured"]["localRerankCandidates"] = Value::Null;
    budget["audit"]["approved"]["rerankCandidates"] = Value::Null;
    for field in ["rerankRequestedCandidates", "resourceRerankLimit", "rerankBudgetSource"] {
        budget["audit"][field] = Value::Null;
    }
    budget
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn retired_rerank_budget_is_null_without_affecting_active_limits() {
        let suggestions =
            json!({"rerankCandidates":60,"rerankCandidateLimit":20,"candidateLimit":1});
        let original = apply(&json!({"taskType":"research","suggestions":suggestions})).unwrap();
        let result =
            apply(&json!({"budget":original,"localRerankCandidates":40,"suggestions":suggestions}))
                .unwrap();
        assert!(result["rerankCandidates"].is_null());
        assert!(result["audit"]["configured"]["localRerankCandidates"].is_null());
        assert!(result["audit"]["rerankRequestedCandidates"].is_null());
        assert!(result["audit"]["rerankBudgetSource"].is_null());
        assert_eq!(result["channelCandidates"], original["channelCandidates"]);
        assert_eq!(result["maximumTokens"], original["maximumTokens"]);
        let original_cuts = original["audit"]["earlyCutReasons"].as_array().unwrap();
        let result_cuts = result["audit"]["earlyCutReasons"].as_array().unwrap();
        assert_eq!(result_cuts, original_cuts);
    }

    #[test]
    fn retired_rerank_suggestions_never_create_new_capacity_or_errors() {
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
            assert!(result["rerankCandidates"].is_null(), "{expected}/{source}");
            assert!(result["audit"]["rerankBudgetSource"].is_null());
            assert_eq!(
                result["audit"]["earlyCutReasons"],
                budget["audit"]["earlyCutReasons"]
            );
        }
    }
}

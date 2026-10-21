//! Normalize provider-reported token counts without transport or a second usage ledger.
//! 仅归一化供应商报告的 token 数量，不执行传输，也不建立第二份用量账本。

use serde_json::Value;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct TokenCounts {
    pub(crate) input_tokens: Option<u64>,
    pub(crate) output_tokens: Option<u64>,
}

pub(crate) fn token_counts(protocol: &str, usage: &Value) -> TokenCounts {
    let input = usage["prompt_tokens"]
        .as_u64()
        .or_else(|| usage["input_tokens"].as_u64());
    let output = usage["completion_tokens"]
        .as_u64()
        .or_else(|| usage["output_tokens"].as_u64());
    let (input_tokens, output_tokens) = match protocol {
        "anthropic-messages" => {
            // Anthropic reports uncached input separately; other protocols include cache hits in input.
            // Anthropic 输入数不含缓存读取/创建；其他协议的输入总数已包含缓存，不能重复加算。
            let input = input.and_then(|input| {
                input
                    .checked_add(cache_component(usage, "cache_read_input_tokens")?)?
                    .checked_add(cache_component(usage, "cache_creation_input_tokens")?)
            });
            (input, output)
        }
        // Current adapters already translate these fields; also accept a native Ollama receipt.
        // 当前适配器已转换字段；同时兼容原生 Ollama 回执，缺失仍保持未知而非伪造零值。
        "ollama" => (
            input.or_else(|| usage["prompt_eval_count"].as_u64()),
            output.or_else(|| usage["eval_count"].as_u64()),
        ),
        _ => (input, output),
    };
    TokenCounts { input_tokens, output_tokens }
}

fn cache_component(usage: &Value, name: &str) -> Option<u64> {
    match usage.get(name) {
        None | Some(Value::Null) => Some(0),
        Some(value) => value.as_u64(),
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct CacheBreakdown {
    pub(crate) read_tokens: Option<u64>,
    pub(crate) creation_tokens: Option<u64>,
    pub(crate) uncached_input_tokens: Option<u64>,
}

/// Preserve reported cache facts without treating missing write/read fields as a measured zero.
/// 保留已报告的缓存事实；缺失的写入或读取字段不能冒充实测零值。
pub(crate) fn cache_breakdown(protocol: &str, usage: &Value) -> CacheBreakdown {
    let read = if protocol == "anthropic-messages" {
        usage["cache_read_input_tokens"].as_u64()
    } else {
        usage["prompt_tokens_details"]["cached_tokens"].as_u64()
            .or_else(|| usage["input_tokens_details"]["cached_tokens"].as_u64())
            .or_else(|| usage["prompt_cache_hit_tokens"].as_u64())
    };
    let creation = if protocol == "anthropic-messages" {
        usage["cache_creation_input_tokens"].as_u64()
    } else {
        usage["input_tokens_details"]["cache_write_tokens"].as_u64()
            .or_else(|| usage["prompt_tokens_details"]["cache_write_tokens"].as_u64())
    };
    let input = token_counts(protocol, usage).input_tokens;
    let read = read.filter(|read| input.is_none_or(|input| *read <= input));
    let creation = creation.filter(|creation| input.is_none_or(|input| *creation <= input)
        && input.zip(read).is_none_or(|(input, read)| read.checked_add(*creation).is_some_and(|sum| sum <= input)));
    let uncached = if protocol == "anthropic-messages" {
        usage["input_tokens"].as_u64()
    } else if let Some(misses) = usage["prompt_cache_miss_tokens"].as_u64() {
        // Misses include cache creation; split a reported write subset without counting those tokens twice.
        // 未命中量包含缓存创建；仅在写入子集已知时扣除，避免同一输入 token 被重复结算。
        input.filter(|input| misses <= *input && read.is_none_or(|read| read.checked_add(misses) == Some(*input)))
            .and_then(|_| match creation { Some(creation) => misses.checked_sub(creation), None => Some(misses) })
    } else {
        input.and_then(|input| input.checked_sub(read?).and_then(|remaining| remaining.checked_sub(creation?)))
    };
    // Partial provider receipts can lack a total; inconsistent subsets still must not fail task settlement.
    // 部分供应商回执可能缺少总量；互相溢出的子集降为未知，不能因统计异常阻断任务结算。
    if [read, creation, uncached].into_iter().flatten().try_fold(0_u64, u64::checked_add).is_none() {
        return CacheBreakdown::default();
    }
    CacheBreakdown { read_tokens: read, creation_tokens: creation, uncached_input_tokens: uncached }
}

/// Existing callers keep their read/write tuple while richer projections consume the same normalization.
/// 旧调用端保留读取/写入二元合同；更完整的投影复用同一归一化来源。
#[cfg(test)]
pub(crate) fn cache_counts(protocol: &str, usage: &Value) -> (Option<u64>, Option<u64>) {
    let counts = cache_breakdown(protocol, usage);
    (counts.read_tokens, counts.creation_tokens)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn cache_projection_counts_explicit_zero_but_never_missing_or_impossible_cache() {
        assert_eq!(cache_counts("openai-completions", &json!({"prompt_tokens":100,
            "prompt_tokens_details":{"cached_tokens":0}})), (Some(0), None));
        assert_eq!(cache_counts("openai-completions", &json!({"prompt_tokens":100})), (None, None));
        assert_eq!(cache_counts("openai-completions", &json!({"prompt_tokens":100,
            "prompt_cache_hit_tokens":101})), (None, None));
        assert_eq!(cache_counts("anthropic-messages", &json!({"input_tokens":10,
            "cache_read_input_tokens":80,"cache_creation_input_tokens":20})), (Some(80), Some(20)));
        assert_eq!(cache_counts("openai-responses", &json!({"input_tokens":100,
            "input_tokens_details":{"cached_tokens":80}})), (Some(80), None));
    }

    #[test]
    fn cache_breakdown_keeps_inclusive_openai_write_and_deepseek_miss_semantics() {
        assert_eq!(cache_breakdown("openai-responses", &json!({"input_tokens":100,
            "input_tokens_details":{"cached_tokens":70,"cache_write_tokens":20}})),
            CacheBreakdown { read_tokens:Some(70), creation_tokens:Some(20), uncached_input_tokens:Some(10) });
        assert_eq!(token_counts("openai-responses", &json!({"input_tokens":100,
            "input_tokens_details":{"cached_tokens":70,"cache_write_tokens":20}})).input_tokens, Some(100));
        assert_eq!(cache_breakdown("openai-completions", &json!({"prompt_tokens":100,
            "prompt_cache_hit_tokens":80,"prompt_cache_miss_tokens":20})),
            CacheBreakdown { read_tokens:Some(80), creation_tokens:None, uncached_input_tokens:Some(20) });
        assert_eq!(cache_breakdown("openai-completions", &json!({"prompt_tokens":100,
            "prompt_cache_hit_tokens":80,"prompt_cache_miss_tokens":20,
            "prompt_tokens_details":{"cache_write_tokens":10}})),
            CacheBreakdown { read_tokens:Some(80), creation_tokens:Some(10), uncached_input_tokens:Some(10) });
        let missing = cache_breakdown("openai-responses", &json!({"input_tokens":100,
            "input_tokens_details":{"cached_tokens":80}}));
        assert_eq!(missing.creation_tokens, None);
        assert_eq!(missing.uncached_input_tokens, None);
        assert_eq!(cache_breakdown("anthropic-messages", &json!({"input_tokens":10,
            "cache_read_input_tokens":70,"cache_creation_input_tokens":20})),
            CacheBreakdown { read_tokens:Some(70), creation_tokens:Some(20), uncached_input_tokens:Some(10) });
    }

    #[test]
    fn impossible_cache_components_are_not_projected_as_billable_facts() {
        let impossible = cache_breakdown("openai-responses", &json!({"input_tokens":100,
            "input_tokens_details":{"cached_tokens":80,"cache_write_tokens":30}}));
        assert_eq!(impossible.creation_tokens, None); assert_eq!(impossible.uncached_input_tokens, None);
        assert_eq!(cache_breakdown("openai-completions", &json!({"prompt_tokens":100,
            "prompt_cache_hit_tokens":80,"prompt_cache_miss_tokens":30})).uncached_input_tokens, None);
        assert_eq!(cache_breakdown("openai-responses", &Value::Null), CacheBreakdown::default());
        assert_eq!(cache_breakdown("anthropic-messages", &json!({"cache_read_input_tokens":80})),
            CacheBreakdown { read_tokens:Some(80), creation_tokens:None, uncached_input_tokens:None });
    }

    #[test]
    fn overflowing_partial_cache_components_remain_unknown_without_a_settlement_error() {
        for (protocol, usage) in [
            ("openai-responses",json!({"input_tokens_details":{"cached_tokens":u64::MAX,"cache_write_tokens":1}})),
            ("anthropic-messages",json!({"cache_read_input_tokens":u64::MAX,"cache_creation_input_tokens":1})),
            ("anthropic-messages",json!({"input_tokens":u64::MAX,"cache_read_input_tokens":1})),
        ] {
            assert_eq!(cache_breakdown(protocol,&usage),CacheBreakdown::default());
        }
    }

    #[test]
    fn anthropic_input_includes_reported_cache_reads_and_creation() {
        assert_eq!(
            token_counts("anthropic-messages", &json!({
                "input_tokens": 10, "output_tokens": 5,
                "cache_read_input_tokens": 80, "cache_creation_input_tokens": 20
            })),
            TokenCounts { input_tokens: Some(110), output_tokens: Some(5) }
        );
    }

    #[test]
    fn inclusive_input_protocols_do_not_add_cache_or_reasoning_subsets_again() {
        for protocol in ["openai-completions", "deepseek", "openai-responses"] {
            assert_eq!(
                token_counts(protocol, &json!({
                    "input_tokens": 100, "output_tokens": 5,
                    "input_tokens_details": { "cached_tokens": 80 },
                    "output_tokens_details": { "reasoning_tokens": 3 },
                    "prompt_cache_hit_tokens": 80, "prompt_cache_miss_tokens": 20,
                    "cache_read_input_tokens": 80, "cache_creation_input_tokens": 20
                })),
                TokenCounts { input_tokens: Some(100), output_tokens: Some(5) }
            );
        }
        assert_eq!(
            token_counts("openai-completions", &json!({
                "prompt_tokens": 100, "completion_tokens": 5,
                "prompt_tokens_details": { "cached_tokens": 80 }
            })),
            TokenCounts { input_tokens: Some(100), output_tokens: Some(5) }
        );
    }

    #[test]
    fn real_zero_and_missing_input_remain_distinct() {
        assert_eq!(
            token_counts("anthropic-messages", &json!({ "input_tokens": 0, "output_tokens": 0 })),
            TokenCounts { input_tokens: Some(0), output_tokens: Some(0) }
        );
        assert_eq!(
            token_counts("anthropic-messages", &json!({
                "output_tokens": 0, "cache_read_input_tokens": 80, "total_tokens": 80
            })),
            TokenCounts { input_tokens: None, output_tokens: Some(0) }
        );
        assert_eq!(token_counts("openai-completions", &Value::Null), TokenCounts::default());
    }

    #[test]
    fn overflow_or_invalid_cache_counts_preserve_unknown_input() {
        for usage in [
            json!({ "input_tokens": u64::MAX, "cache_read_input_tokens": 1, "output_tokens": 2 }),
            json!({ "input_tokens": u64::MAX - 1, "cache_read_input_tokens": 1, "cache_creation_input_tokens": 1, "output_tokens": 2 }),
            json!({ "input_tokens": 1, "cache_read_input_tokens": -1, "output_tokens": 2 }),
        ] {
            assert_eq!(
                token_counts("anthropic-messages", &usage),
                TokenCounts { input_tokens: None, output_tokens: Some(2) }
            );
        }
    }

    #[test]
    fn ollama_accepts_current_adapter_counts_and_native_counts_without_summing() {
        for usage in [
            json!({ "prompt_tokens": 3, "completion_tokens": 5 }),
            json!({ "prompt_eval_count": 3, "eval_count": 5 }),
            json!({ "prompt_tokens": 3, "completion_tokens": 5, "prompt_eval_count": 3, "eval_count": 5 }),
        ] {
            assert_eq!(
                token_counts("ollama", &usage),
                TokenCounts { input_tokens: Some(3), output_tokens: Some(5) }
            );
        }
        assert_eq!(
            token_counts("ollama", &json!({ "eval_count": 0 })),
            TokenCounts { input_tokens: None, output_tokens: Some(0) }
        );
    }
}

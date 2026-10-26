//! Offline snapshot diagnostics and explicit-rate accounting; never an API client or production policy.
//! 离线请求快照诊断及显式价目核算；不是 API 客户端或生产策略，不读取正式数据目录。

use std::collections::{BTreeMap, HashMap, HashSet, VecDeque};

use chrono::{DateTime, NaiveDate};
use serde_json::{Value, json};

use super::cache_diagnostics::PrefixDiagnostics;

const MAXIMUM_REPLAY_SCOPES: usize = 64;
const MAXIMUM_OBSERVATIONS: usize = 4096;
const MAXIMUM_TOOL_NAMES: usize = 4096;
const MAXIMUM_CONTROL_BYTES: usize = 512 * 1024;

#[derive(Default)]
struct StrategyTotals {
    records: u64,
    comparisons: u64,
    baseline_resets: u64,
    incomplete: u64,
    prefix_breaks: u64,
    retained_tokens: u64,
    input_tokens: u64,
}

impl StrategyTotals {
    fn observe(&mut self, prefix: &Value) {
        self.records = self.records.saturating_add(1);
        if prefix["state"] == "incomplete" { self.incomplete = self.incomplete.saturating_add(1); }
        if prefix["breakAt"] == "baseline" { self.baseline_resets = self.baseline_resets.saturating_add(1); }
        else {
            self.comparisons = self.comparisons.saturating_add(1);
            if prefix["breakAt"] != "none" { self.prefix_breaks = self.prefix_breaks.saturating_add(1); }
        }
        self.retained_tokens = self.retained_tokens.saturating_add(prefix["retainedTokensEstimate"].as_u64().unwrap_or(0));
        self.input_tokens = self.input_tokens.saturating_add(prefix["inputTokensEstimate"].as_u64().unwrap_or(0));
    }

    fn report(&self) -> Value {
        json!({"observedRecords":self.records,"comparisons":self.comparisons,"baselineResets":self.baseline_resets,
            "incompleteRecords":self.incomplete,"prefixBreaks":self.prefix_breaks,
            "retainedTokensEstimate":self.retained_tokens,"inputTokensEstimate":self.input_tokens,
            "actualProviderCacheReadRate":Value::Null,"actualCost":Value::Null,
            "method":"relative complete-message-boundary structural estimate; not a cache-hit or cost forecast"})
    }
}

#[derive(Default)]
struct StableToolOrder {
    previous: HashMap<&'static str, Vec<String>>,
}

fn declaration_name<'a>(field: &str, tool: &'a Value) -> Option<&'a str> {
    let custom_type = tool["type"].is_null() || tool["type"] == "function";
    let name = if field == "functions" { tool["name"].as_str() }
        else if custom_type && tool["function"].is_object() { tool["function"]["name"].as_str() }
        else if custom_type { tool["name"].as_str() } else { None };
    name.filter(|name| !name.is_empty() && name.len() <= 512)
}

impl StableToolOrder {
    /// Reorder current declarations; apply removals and schema changes immediately.
    /// 仅重排当前声明；删除与定义变更立即生效。
    fn project(&mut self, body: &Value) -> Result<Value, &'static str> {
        let mut projected = body.clone();
        for field in ["tools", "functions"] {
            let Some(value) = body.get(field).filter(|value| !value.is_null()) else {
                self.previous.remove(field); continue;
            };
            let tools = value.as_array().ok_or("declarations-not-an-array")?;
            if tools.len() > MAXIMUM_TOOL_NAMES { return Err("offline-tool-order-diagnostic-capacity"); }
            let mut available = BTreeMap::new();
            let mut name_bytes = 0usize;
            for tool in tools {
                let name = declaration_name(field, tool).ok_or("unsupported-or-unnamed-declaration")?;
                name_bytes = name_bytes.saturating_add(name.len());
                if name_bytes > 128 * 1024 { return Err("offline-tool-name-diagnostic-capacity"); }
                if available.insert(name.to_owned(), tool.clone()).is_some() { return Err("duplicate-tool-identity"); }
            }
            let mut ordered = Vec::with_capacity(tools.len());
            let mut names = Vec::with_capacity(tools.len());
            for name in self.previous.get(field).into_iter().flatten() {
                if let Some(tool) = available.remove(name) { names.push(name.clone()); ordered.push(tool); }
            }
            for (name, tool) in available { names.push(name); ordered.push(tool); }
            self.previous.insert(field, names);
            projected[field] = json!(ordered);
        }
        Ok(projected)
    }
}

#[derive(Clone)]
struct MessageSource {
    id: String,
    control_key: Option<String>,
    supersedes: Option<String>,
}

#[derive(Default)]
struct ControlProjection {
    append_history: Vec<(MessageSource, Value)>,
    active_controls: BTreeMap<String, MessageSource>,
}

fn bounded_label(value: &Value) -> Option<&str> {
    value.as_str().filter(|value| !value.is_empty() && value.len() <= 512)
}

fn message_sources(record: &Value, messages: &[Value]) -> Result<Vec<MessageSource>, &'static str> {
    let sources = record["messageSources"].as_array().filter(|sources| sources.len() == messages.len())
        .ok_or("explicit-message-sources-required-for-every-message")?;
    if messages.len() > MAXIMUM_OBSERVATIONS { return Err("offline-control-message-diagnostic-capacity"); }
    let mut identifiers = HashSet::new();
    sources.iter().zip(messages).map(|(source, message)| {
        let id = bounded_label(&source["id"]).ok_or("stable-message-source-id-required")?;
        if !identifiers.insert(id.to_owned()) { return Err("duplicate-message-source-id"); }
        if source["immediateInvalidation"] == true { return Err("immediate-invalidation-resets-control-comparison"); }
        let control_key = match source["kind"].as_str() {
            Some("history") => None,
            Some("control") => {
                if source["cacheMutationSafe"] != true || message["role"] != "user" || !message["content"].is_string()
                    || !message["tool_calls"].is_null() || !message["function_call"].is_null() {
                    return Err("only-explicit-safe-user-control-notices-can-be-repositioned");
                }
                Some(bounded_label(&source["controlKey"]).ok_or("control-key-required")?.to_owned())
            }
            _ => return Err("explicit-history-or-control-source-kind-required"),
        };
        let supersedes = source.get("supersedesId").map(|value|
            bounded_label(value).map(str::to_owned).ok_or("invalid-control-supersession-id")).transpose()?;
        Ok(MessageSource { id:id.to_owned(), control_key, supersedes })
    }).collect()
}

impl ControlProjection {
    /// These offline alternatives require source identities and explicit supersession; message text is never classified.
    /// 离线两路比较要求来源身份与明确取代关系，不猜测用户正文；删除、改写或敏感失效立即重建基线。
    fn project(&mut self, record: &Value) -> Result<(Value, Value), &'static str> {
        let body = &record["request"];
        let field = if body["messages"].is_array() { "messages" }
            else if body["input"].is_array() { "input" } else { return Err("explicit-message-array-required"); };
        let messages = body[field].as_array().unwrap();
        let sources = message_sources(record, messages)?;
        if serde_json::to_vec(&body[field]).map_or(true, |bytes| bytes.len() > MAXIMUM_CONTROL_BYTES) {
            return Err("offline-control-history-diagnostic-capacity");
        }
        let previous_by_id = self.append_history.iter().map(|(source, message)| (source.id.as_str(), message)).collect::<HashMap<_, _>>();
        let mut latest: BTreeMap<String, MessageSource> = BTreeMap::new();
        for source in &sources {
            if let Some(key) = &source.control_key {
                if latest.get(key).is_some_and(|previous| source.supersedes.as_ref() != Some(&previous.id)) {
                    return Err("control-versions-need-explicit-current-supersession-chain");
                }
                latest.insert(key.clone(), source.clone());
            }
        }
        let mut current_formal = sources.iter().zip(messages).filter(|(source, _)| source.control_key.is_none());
        for (left_source, left_message) in self.append_history.iter().filter(|(source, _)| source.control_key.is_none()) {
            let Some((right_source, right_message)) = current_formal.next() else {
                return Err("formal-history-deleted-rewritten-or-reordered");
            };
            if left_source.id != right_source.id || left_message != right_message {
                return Err("formal-history-deleted-rewritten-or-reordered");
            }
        }
        for (source, message) in sources.iter().zip(messages) {
            if previous_by_id.get(source.id.as_str()).is_some_and(|previous| **previous != *message) {
                return Err("message-content-changed-under-stable-id");
            }
        }
        for (key, previous) in &self.active_controls {
            let Some(current) = latest.get(key) else { return Err("control-disappeared-without-explicit-retirement"); };
            if current.id != previous.id && current.supersedes.as_ref() != Some(&previous.id) {
                return Err("changed-control-needs-explicit-supersession");
            }
        }
        let new_messages = sources.iter().zip(messages).filter(|(source, _)| !previous_by_id.contains_key(source.id.as_str()))
            .map(|(source, message)| (source.clone(), message.clone())).collect::<Vec<_>>();
        let mut append = self.append_history.clone();
        append.extend(new_messages);
        let append_values = append.iter().map(|(_, message)| message.clone()).collect::<Vec<_>>();
        if serde_json::to_vec(&append_values).map_or(true, |bytes| bytes.len() > MAXIMUM_CONTROL_BYTES) {
            return Err("offline-control-history-diagnostic-capacity");
        }
        let mut emitted = HashSet::new();
        let mut replacements = Vec::with_capacity(messages.len());
        for (source, message) in sources.iter().zip(messages) {
            if let Some(key) = &source.control_key {
                if emitted.insert(key.clone()) {
                    let current_id = &latest[key].id;
                    let position = sources.iter().position(|source| &source.id == current_id).unwrap();
                    replacements.push(messages[position].clone());
                }
            } else { replacements.push(message.clone()); }
        }
        let mut append_body = body.clone(); append_body[field] = json!(append_values);
        let mut replace_body = body.clone(); replace_body[field] = json!(replacements);
        self.append_history = append; self.active_controls = latest;
        Ok((append_body, replace_body))
    }
}

#[derive(Default)]
struct ReplayScope {
    original: PrefixDiagnostics,
    stable_tools: PrefixDiagnostics,
    tool_order: StableToolOrder,
    control_append: PrefixDiagnostics,
    control_replace: PrefixDiagnostics,
    controls: ControlProjection,
}

#[derive(Default)]
struct CalibrationGroup {
    actual_records: u64,
    input_tokens: u64,
    cache_read_tokens: u64,
    comparison_records: u64,
    difference_total: f64,
    absolute_difference_total: f64,
}

/// Analyze saved requests within limits; callers handle files, without tool or provider replay.
/// 有界分析已保存请求；文件由调用方读取，不重放工具或供应商请求。
pub struct ReplayAnalyzer {
    scopes: HashMap<String, ReplayScope>,
    order: VecDeque<String>,
    observations: Vec<Value>,
    records: u64,
    relative_only_records: u64,
    eligible_records: u64,
    cache_read_tokens: u64,
    cache_input_tokens: u64,
    baseline_resets: u64,
    prefix_breaks: u64,
    estimated_retained_tokens: u64,
    estimated_input_tokens: u64,
    priced_records: u64,
    total_cost: f64,
    currency: Option<String>,
    incomplete: Vec<&'static str>,
    stable_tools: StrategyTotals,
    control_append: StrategyTotals,
    control_replace: StrategyTotals,
    calibration_groups: BTreeMap<(String, String), CalibrationGroup>,
    epoch_covered_records: u64,
    tool_strategy_skipped: u64,
    control_strategy_skipped: u64,
}

impl Default for ReplayAnalyzer {
    fn default() -> Self {
        Self { scopes: HashMap::new(), order: VecDeque::new(), observations: Vec::new(), records: 0,
            relative_only_records: 0, eligible_records: 0, cache_read_tokens: 0, cache_input_tokens: 0,
            baseline_resets: 0, prefix_breaks: 0, estimated_retained_tokens: 0, estimated_input_tokens: 0,
            priced_records: 0, total_cost: 0.0, currency: None, incomplete: Vec::new(),
            stable_tools: StrategyTotals::default(), control_append: StrategyTotals::default(), control_replace: StrategyTotals::default(),
            calibration_groups: BTreeMap::new(), epoch_covered_records:0, tool_strategy_skipped:0, control_strategy_skipped:0 }
    }
}

fn select_prices<'a>(prices: Option<&'a Value>, protocol: &str, model: &str) -> Option<&'a Value> {
    let prices = prices?;
    let collection = prices.as_array().or_else(|| prices["pricebooks"].as_array());
    if let Some(collection) = collection {
        collection.iter().find(|price| price["protocol"] == protocol && price["model"] == model)
    } else { Some(prices) }
}

fn date_eligible(record: &Value) -> bool {
    // Use the record date, not the current time or file modification time.
    // 使用记录日期，不以当前时间或文件修改时间代替。
    record["recordedAt"].as_str().and_then(|value| DateTime::parse_from_rfc3339(value).ok())
        .is_some_and(|timestamp| timestamp.date_naive() >= NaiveDate::from_ymd_opt(2026, 10, 21).unwrap())
}

fn rate(prices: &Value, name: &str) -> Option<f64> {
    prices[name].as_f64().filter(|value| value.is_finite() && *value >= 0.0)
}

fn cost(protocol: &str, model: &str, usage: &Value, prices: &Value) -> Value {
    if prices["protocol"].as_str() != Some(protocol) || prices["model"].as_str() != Some(model) {
        return json!({"state":"incomplete","reason":"explicit-model-protocol-price-match-required"});
    }
    let Some(currency) = prices["currency"].as_str().filter(|value| !value.is_empty()) else {
        return json!({"state":"incomplete","reason":"currency-required"});
    };
    let counts = super::usage::token_counts(protocol, usage);
    let cache = super::usage::cache_breakdown(protocol, usage);
    let (Some(input), Some(output), Some(read)) = (counts.input_tokens, counts.output_tokens, cache.read_tokens) else {
        return json!({"state":"incomplete","reason":"reported-input-output-cache-read-required"});
    };
    let Some((input_rate, read_rate, output_rate)) = rate(prices,"uncachedInputPerMillion")
        .zip(rate(prices,"cacheReadPerMillion")).zip(rate(prices,"outputPerMillion"))
        .map(|((input, read), output)|(input, read, output)) else {
        return json!({"state":"incomplete","reason":"finite-explicit-rates-required"});
    };
    let (ordinary, write_charge) = match prices["cacheWriteBilling"].as_str() {
        Some("included-in-input") => match input.checked_sub(read) {
            Some(ordinary) => (ordinary, 0.0),
            None => return json!({"state":"incomplete","reason":"inconsistent-token-subsets"}),
        },
        Some("separate") => {
            let (Some(write), Some(ordinary), Some(write_rate)) =
                (cache.creation_tokens, cache.uncached_input_tokens, rate(prices,"cacheCreationPerMillion")) else {
                return json!({"state":"incomplete","reason":"separate-write-billing-requires-reported-write-and-ordinary-subsets"});
            };
            if ordinary.checked_add(read).and_then(|sum|sum.checked_add(write)) != Some(input) {
                return json!({"state":"incomplete","reason":"inconsistent-token-subsets"});
            }
            (ordinary, write as f64 * write_rate / 1_000_000.0)
        }
        _ => return json!({"state":"incomplete","reason":"explicit-cache-write-billing-required"}),
    };
    let ordinary_charge = ordinary as f64 * input_rate / 1_000_000.0;
    let read_charge = read as f64 * read_rate / 1_000_000.0;
    let output_charge = output as f64 * output_rate / 1_000_000.0;
    let total = ordinary_charge + read_charge + write_charge + output_charge;
    if !total.is_finite() { return json!({"state":"incomplete","reason":"cost-overflow"}); }
    json!({"state":"calculated","currency":currency,"total":total,"priceSource":"explicit-offline-input-not-current-vendor-price",
        "cacheWriteBilling":prices["cacheWriteBilling"],"reportedInputTokens":input,"reportedOutputTokens":output,
        "reportedCacheReadTokens":read,"reportedCacheCreationTokens":cache.creation_tokens,
        "billedOrdinaryInputTokens":ordinary,"components":{"ordinaryInput":ordinary_charge,"cacheRead":read_charge,
            "cacheCreation":write_charge,"output":output_charge}})
}

/// Compare only an explicitly supplied offline mutation scenario; never delay required live corrections.
/// 仅比较显式提供的离线变更情景；不执行在线调度，不延迟权限、记忆或正确性要求的即时修正。
pub fn mutation_cost_advice(protocol: &str, model: &str, scenario: &Value, prices: &Value) -> Value {
    if prices["protocol"].as_str() != Some(protocol) || prices["model"].as_str() != Some(model) {
        return json!({"state":"incomplete","reason":"explicit-model-protocol-price-match-required"});
    }
    let Some(currency)=prices["currency"].as_str().filter(|currency| !currency.is_empty()) else {
        return json!({"state":"incomplete","reason":"currency-required"});
    };
    let (Some(invalidated),Some(removable),Some(remaining))=(scenario["invalidatedCachedTokens"].as_u64(),
        scenario["removableTokens"].as_u64(),scenario["remainingRequestsEstimate"].as_u64()) else {
        return json!({"state":"incomplete","reason":"explicit-invalidated-removable-and-future-request-estimates-required"});
    };
    let assumptions=&scenario["cacheResidencyAssumption"];
    let fraction=|name|rate(assumptions,name).filter(|fraction| *fraction<=1.0);
    let (Some(existing_hit),Some(future_hit),Some(creation_fraction))=(fraction("existingPrefixHitFraction"),
        fraction("futurePrefixHitFraction"),fraction("creationFractionOnMiss")) else {
        return json!({"state":"incomplete","reason":"explicit-residency-and-creation-fractions-between-zero-and-one-required"});
    };
    let (Some(ordinary_rate),Some(read_rate))=(rate(prices,"uncachedInputPerMillion"),rate(prices,"cacheReadPerMillion")) else {
        return json!({"state":"incomplete","reason":"explicit-finite-input-and-cache-read-prices-required"});
    };
    let (miss_rate,creation_billing)=match prices["cacheWriteBilling"].as_str() {
        Some("included-in-input")=>(ordinary_rate,"cache creation included in ordinary input price"),
        Some("separate")=>{
            let Some(write_rate)=rate(prices,"cacheCreationPerMillion") else {
                return json!({"state":"incomplete","reason":"separate-cache-creation-price-required"});
            };
            // A write subset replaces its ordinary-input charge; it is never billed twice.
            // 缓存创建子集使用独立创建价替代普通输入价，不能对同一 token 同时加收两项。
            ((1.0-creation_fraction)*ordinary_rate+creation_fraction*write_rate,
                "cache creation priced explicitly for the assumed fraction of every miss")
        }
        _=>return json!({"state":"incomplete","reason":"explicit-cache-write-billing-required"}),
    };
    let baseline_rate=existing_hit*read_rate+(1.0-existing_hit)*miss_rate;
    let future_rate=future_hit*read_rate+(1.0-future_hit)*miss_rate;
    let additional_rebuild_cost=invalidated as f64*(miss_rate-baseline_rate)/1_000_000.0;
    let future_savings=removable as f64*remaining as f64*future_rate/1_000_000.0;
    let net_savings=future_savings-additional_rebuild_cost;
    if ![additional_rebuild_cost,future_savings,net_savings].iter().all(|value|value.is_finite()) {
        return json!({"state":"incomplete","reason":"scenario-cost-overflow"});
    }
    let cost_comparison=if net_savings>0.0 {"estimated-cheaper-to-rebuild"}
        else if net_savings<0.0 {"estimated-cheaper-to-keep-prefix"}else{"estimated-tie"};
    json!({"state":"estimate","currency":currency,"rebuildAdditionalInputCostEstimate":additional_rebuild_cost,
        "futureInputSavingsEstimate":future_savings,"netInputSavingsEstimate":net_savings,"costComparison":cost_comparison,
        "recommendation":if scenario["correctnessRequired"]==true {"apply-required-correction-independent-of-cost"}else{cost_comparison},
        "assumptions":{"source":"explicit caller assumptions, not measured residency or a calibrated predictor",
            "invalidatedCachedTokens":invalidated,"removableTokens":removable,"remainingRequestsEstimate":remaining,
            "existingPrefixHitFraction":existing_hit,"futurePrefixHitFraction":future_hit,"creationFractionOnMiss":creation_fraction,
            "horizon":"future requests strictly after rebuild; invalidated tokens exclude removed material",
            "creationBilling":creation_billing,"priceHorizon":"supplied explicit prices assumed unchanged"},
        "excluded":["savings in the rebuild request itself","cache storage and residency service fees",
            "output changes, tool choice, task quality, extra model calls and latency"],
        "policy":"offline advice only; required permission, memory, privacy and correctness updates take effect immediately regardless of estimated cost"})
}

impl ReplayAnalyzer {
    fn incomplete(&mut self, reason: &'static str) {
        if !self.incomplete.contains(&reason) { self.incomplete.push(reason); }
    }

    /// JSONL row: recordedAt, taskId, protocol, model, request, usage, usageSource and snapshot completeness.
    /// JSONL 行包含记录日期、任务范围、协议、模型、真实请求、供应商用量来源和快照完整性声明。
    pub fn observe(&mut self, record: &Value, prices: Option<&Value>) {
        self.records += 1;
        let protocol = record["protocol"].as_str().unwrap_or("");
        let model = record["model"].as_str().unwrap_or("");
        let prices = select_prices(prices, protocol, model);
        let counts = super::usage::token_counts(protocol, &record["usage"]);
        let cache = super::usage::cache_breakdown(protocol, &record["usage"]);
        // Anthropic's ordinary input excludes both cache subsets; an absent creation count is unknown, not zero.
        // Anthropic 普通输入不含缓存读写；创建量缺失时总输入未证实，不能猜零后报告精确命中率。
        let complete_input = protocol != "anthropic-messages"
            || cache.creation_tokens.is_some() && cache.uncached_input_tokens.is_some();
        let eligible = date_eligible(record) && record["usageSource"] == "provider"
            && cache.read_tokens.zip(counts.input_tokens).is_some_and(|(read,input)| read <= input) && complete_input;
        let mut observation = json!({"recordNumber":self.records,"comparison":"relative-structure-only"});
        if eligible {
            self.eligible_records += 1;
            let input=counts.input_tokens.unwrap(); let read=cache.read_tokens.unwrap();
            self.cache_input_tokens=self.cache_input_tokens.saturating_add(input);
            self.cache_read_tokens=self.cache_read_tokens.saturating_add(read);
            observation["reportedProviderUsage"]=json!({"inputTokens":input,"cacheReadTokens":read,
                "cacheCreationTokens":cache.creation_tokens,"ordinaryInputTokens":cache.uncached_input_tokens,"outputTokens":counts.output_tokens});
            observation["comparison"]=json!("relative-structure-plus-reported-provider-usage");
            let pricing=prices.map(|prices|cost(protocol, model, &record["usage"],prices))
                .unwrap_or_else(||json!({"state":"incomplete","reason":"explicit-pricebook-unavailable"}));
            if pricing["state"] == "calculated" {
                let currency=pricing["currency"].as_str().unwrap();
                if self.currency.as_ref().is_none_or(|previous|previous.as_str()==currency)
                    && (self.total_cost+pricing["total"].as_f64().unwrap()).is_finite() {
                    self.currency=Some(currency.to_owned()); self.priced_records+=1;
                    self.total_cost+=pricing["total"].as_f64().unwrap();
                } else { self.incomplete("mixed-currencies-not-aggregated"); }
            } else { self.incomplete("some-eligible-records-lack-complete-explicit-cost-inputs"); }
            observation["cost"]=pricing;
        } else {
            self.relative_only_records += 1;
            observation["actualUsageExcluded"]=json!(if !date_eligible(record) {"before-cutoff-or-record-date-missing"}
                else {"actual-provider-cache-read-or-total-not-established"});
        }
        if let Some(scenario)=record.get("mutationScenario") {
            observation["mutationCostAdvice"]=prices.map(|prices|mutation_cost_advice(protocol,model,scenario,prices))
                .unwrap_or_else(||json!({"state":"incomplete","reason":"explicit-pricebook-unavailable"}));
        }
        if record["schemaSnapshotsComplete"] != true || record["historySnapshotComplete"] != true {
            self.incomplete("schema-or-history-snapshots-incomplete");
            observation["snapshotState"]=json!("incomplete");
        }
        let scope=record["taskId"].as_str().filter(|scope| !scope.is_empty() && scope.len()<=512);
        let epoch=bounded_label(&record["diagnosticEpoch"]);
        if epoch.is_some() { self.epoch_covered_records = self.epoch_covered_records.saturating_add(1); }
        else { self.incomplete("diagnostic-epoch-missing-records-start-new-structural-baselines"); }
        let mut calibration_difference = None;
        if let Some(scope)=scope.filter(|_| record["request"].is_object() && !protocol.is_empty() && !model.is_empty()) {
            // A supplied epoch is an explicit comparison boundary. Missing epochs never join two requests.
            // 显式epoch界定可比较范围；缺失时每条记录建立新基线，保留真实计量但不冒充跨重启连续历史。
            let scope_key=json!([scope,protocol,model,epoch,if epoch.is_none(){Some(self.records)}else{None}]).to_string();
            if !self.scopes.contains_key(&scope_key) {
                if self.scopes.len()>=MAXIMUM_REPLAY_SCOPES {
                    if let Some(oldest)=self.order.pop_front() { self.scopes.remove(&oldest); }
                    self.incomplete("scope-capacity-eviction-resets-comparison-baseline");
                }
                self.scopes.insert(scope_key.clone(),ReplayScope::default()); self.order.push_back(scope_key.clone());
            }
            let state=self.scopes.get_mut(&scope_key).unwrap();
            let next=state.original.prepare(&record["connection"],model,protocol,&record["request"],&scope_key);
            let prefix=next.report(); state.original.commit(next);
            let tools = match state.tool_order.project(&record["request"]) {
                Ok(body) => {
                    let next=state.stable_tools.prepare(&record["connection"],model,protocol,&body,&scope_key);
                    let prefix=next.report();state.stable_tools.commit(next);
                    Ok(prefix)
                }
                Err(reason) => {state.tool_order=StableToolOrder::default();state.stable_tools=PrefixDiagnostics::default();Err(reason)}
            };
            let controls = match state.controls.project(record) {
                Ok((append,replace)) => {
                    let next=state.control_append.prepare(&record["connection"],model,protocol,&append,&scope_key);
                    let append=next.report();state.control_append.commit(next);
                    let next=state.control_replace.prepare(&record["connection"],model,protocol,&replace,&scope_key);
                    let replace=next.report();state.control_replace.commit(next);
                    Ok((append,replace))
                }
                Err(reason) => {
                    state.controls=ControlProjection::default();state.control_append=PrefixDiagnostics::default();
                    state.control_replace=PrefixDiagnostics::default();Err(reason)
                }
            };
            if prefix["breakAt"] == "baseline" { self.baseline_resets+=1; }
            else if prefix["breakAt"] != "none" { self.prefix_breaks+=1; }
            self.estimated_retained_tokens=self.estimated_retained_tokens.saturating_add(prefix["retainedTokensEstimate"].as_u64().unwrap_or(0));
            self.estimated_input_tokens=self.estimated_input_tokens.saturating_add(prefix["inputTokensEstimate"].as_u64().unwrap_or(0));
            match tools {
                Ok(tools) => {
                    self.stable_tools.observe(&tools);
                    let original=prefix["retainedTokensEstimate"].as_u64().unwrap_or(0);
                    let stable=tools["retainedTokensEstimate"].as_u64().unwrap_or(0);
                    observation["stableToolsStrategy"]=json!({"state":"relative-structure-only","sameSelectedSet":true,
                        "currentSchemasAndRevocationsPreserved":true,"prefix":tools,
                        "additionalRetainedTokensEstimate":stable.saturating_sub(original),
                        "reducedRetainedTokensEstimate":original.saturating_sub(stable)});
                }
                Err(reason) => {
                    self.tool_strategy_skipped=self.tool_strategy_skipped.saturating_add(1);
                    self.incomplete("some-tool-order-strategy-records-unsupported-or-incomplete");
                    observation["stableToolsStrategy"]=json!({"state":"incomplete","reason":reason});
                }
            }
            match controls {
                Ok((append,replace)) => {
                    self.control_append.observe(&append);self.control_replace.observe(&replace);
                    observation["controlStrategies"]=json!({"state":"relative-structure-only","appendPrefix":append,
                        "replacePrefix":replace,"assumptions":"explicit safe-user-control source metadata; never inferred from message text"});
                }
                Err(reason) => {
                    self.control_strategy_skipped=self.control_strategy_skipped.saturating_add(1);
                    self.incomplete("some-control-strategy-records-lack-valid-explicit-source-metadata");
                    observation["controlStrategies"]=json!({"state":"incomplete","reason":reason});
                }
            }
            if eligible && epoch.is_some() && record["schemaSnapshotsComplete"] == true && record["historySnapshotComplete"] == true
                && prefix["state"] == "compared" && prefix["breakAt"] != "route" {
                if let Some((input,estimated))=counts.input_tokens.zip(prefix["inputTokensEstimate"].as_u64())
                    .filter(|(input,estimated)| *input>0 && *estimated>0) {
                    let measured=cache.read_tokens.unwrap() as f64/input as f64;
                    let structural=prefix["retainedTokensEstimate"].as_u64().unwrap_or(0) as f64/estimated as f64;
                    calibration_difference=Some(measured-structural);
                    observation["calibrationObservation"]=json!({"state":"observed-gap-only-not-a-fitted-prediction",
                        "actualProviderCacheReadFraction":measured,"originalStructuralRetainedFraction":structural,
                        "actualMinusStructuralFraction":measured-structural,
                        "interpretation":"different tokenizers, protocol projection and unknown vendor residency all contribute; not automatic provider-fault attribution"});
                }
            }
            observation["prefix"]=prefix;
        } else { self.incomplete("task-scope-protocol-model-or-request-missing"); }
        if eligible && !protocol.is_empty() && protocol.len()<=512 && !model.is_empty() && model.len()<=512 {
            let key=(protocol.to_owned(),model.to_owned());
            if self.calibration_groups.contains_key(&key) || self.calibration_groups.len()<MAXIMUM_REPLAY_SCOPES {
                let group=self.calibration_groups.entry(key).or_default();
                group.actual_records=group.actual_records.saturating_add(1);
                group.input_tokens=group.input_tokens.saturating_add(counts.input_tokens.unwrap());
                group.cache_read_tokens=group.cache_read_tokens.saturating_add(cache.read_tokens.unwrap());
                if let Some(difference)=calibration_difference {
                    group.comparison_records=group.comparison_records.saturating_add(1);
                    group.difference_total+=difference;group.absolute_difference_total+=difference.abs();
                }
            } else { self.incomplete("model-protocol-group-diagnostic-capacity"); }
        }
        // Without provider tokenization or residency data, stable prefixes do not prove cache hits.
        // 缺少供应商分词和驻留数据，稳定前缀只能比较，不能证明实际命中。
        if self.observations.len()<MAXIMUM_OBSERVATIONS { self.observations.push(observation); }
        else { self.incomplete("public-observation-capacity-summary-continues"); }
    }

    pub fn report(&self) -> Value {
        let groups=self.calibration_groups.iter().map(|((protocol,model),group)| json!({
            "protocol":protocol,"model":model,"eligibleProviderUsageRecords":group.actual_records,
            "actualProviderInputTokens":group.input_tokens,"actualProviderCacheReadTokens":group.cache_read_tokens,
            "actualProviderCacheReadRate":if group.input_tokens>0{json!(group.cache_read_tokens as f64/group.input_tokens as f64)}else{Value::Null},
            "pairedOriginalStructureObservations":group.comparison_records,
            "meanActualMinusStructuralFraction":if group.comparison_records>0{json!(group.difference_total/group.comparison_records as f64)}else{Value::Null},
            "meanAbsoluteFractionGap":if group.comparison_records>0{json!(group.absolute_difference_total/group.comparison_records as f64)}else{Value::Null},
            "comparisonMeaning":"original-request observed gaps only; not stable-tool/control strategy hit-rate forecasts"})).collect::<Vec<_>>();
        json!({"schemaVersion":1,"mode":"offline-saved-wire-snapshots-only","state":"incomplete",
            "recordCount":self.records,"relativeOnlyRecords":self.relative_only_records,"eligibleProviderUsageRecords":self.eligible_records,
            "actualUsageEligibility":"recordedAt calendar date >= 2026-10-21, usageSource=provider, explicit cache read and total input",
            "structure":{"baselineResets":self.baseline_resets,"prefixBreaks":self.prefix_breaks,
                "retainedTokensEstimate":self.estimated_retained_tokens,"inputTokensEstimate":self.estimated_input_tokens,
                "method":"complete-message-boundary structural estimate; no vendor tokenizer, TTL or cache block assumptions"},
            "actualProviderCacheReadRate":if self.cache_input_tokens>0 {json!(self.cache_read_tokens as f64/self.cache_input_tokens as f64)}else{Value::Null},
            "actualProviderInputTokens":self.cache_input_tokens,"actualProviderCacheReadTokens":self.cache_read_tokens,
            "cost":{"state":if self.priced_records==self.eligible_records && self.priced_records>0 {"calculated"}else{"incomplete"},
                "pricedRecords":self.priced_records,"eligibleRecords":self.eligible_records,"currency":self.currency,
                "knownRecordsSubtotal":if self.priced_records>0{json!(self.total_cost)}else{Value::Null},
                "note":"only explicitly priced eligible records; excludes missing prices, residency/storage and other service charges"},
            "calibration":{"state":"incomplete","reason":"provider-tokenizer-and-cache-residency-unavailable; no predicted-hit-rate or strategy-success claim",
                "epochCoveredRecords":self.epoch_covered_records,"modelProtocolGroups":groups,
                "coverage":"actual receipts retained even without epoch; paired gaps require explicit epoch and complete schema/history snapshots"},
            "strategyComparison":{"state":"relative-structure-only","original":{
                "prefixBreaks":self.prefix_breaks,"baselineResets":self.baseline_resets,
                "retainedTokensEstimate":self.estimated_retained_tokens,"inputTokensEstimate":self.estimated_input_tokens},
                "stableSelectedToolOrder":self.stable_tools.report(),"controlAppend":self.control_append.report(),
                "controlReplace":self.control_replace.report(),"toolStrategySkippedRecords":self.tool_strategy_skipped,
                "controlStrategySkippedRecords":self.control_strategy_skipped,
                "toolPolicy":"same selected set and current definitions; survivors keep relative order, new names deterministic, revoked names removed immediately",
                "controlPolicy":"explicit safe-user-control metadata and supersession only; formal history, role and tool-call/receipt order unchanged; immediate invalidation resets",
                "accounting":"real provider usage and explicitly priced costs counted only once for original requests; hypothetical strategies have no actual-rate or actual-cost claims"},
            "incompleteReasons":self.incomplete,"observations":self.observations})
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(date: &str, usage: Value) -> Value {
        json!({"recordedAt":date,"taskId":"fixture","protocol":"openai-completions","model":"fixture",
            "request":{"messages":[{"role":"user","content":"PRIVATE_CONTENT"}]},"usageSource":"provider","usage":usage,
            "schemaSnapshotsComplete":true,"historySnapshotComplete":true})
    }

    #[test]
    fn offline_replay_never_reports_historical_or_missing_date_usage_as_current_cache_hits() {
        let usage=json!({"prompt_tokens":100,"completion_tokens":10,"prompt_cache_hit_tokens":80,"prompt_cache_miss_tokens":20});
        let mut replay=ReplayAnalyzer::default();
        replay.observe(&row("2026-10-20T23:59:59+08:00",usage.clone()),None);
        replay.observe(&row("not-a-date",usage.clone()),None);
        assert!(replay.report()["actualProviderCacheReadRate"].is_null());
        replay.observe(&row("2026-10-21T00:00:00+08:00",usage),None);
        let report=replay.report(); assert_eq!(report["eligibleProviderUsageRecords"],1);
        assert_eq!(report["actualProviderCacheReadRate"],0.8); assert_eq!(report["relativeOnlyRecords"],2);
        assert!(!report.to_string().contains("PRIVATE_CONTENT")); assert_eq!(report["calibration"]["state"],"incomplete");
        let mut partial=row("2026-10-21T12:00:00+08:00",json!({"input_tokens":20,"output_tokens":5,"cache_read_input_tokens":80}));
        partial["protocol"]=json!("anthropic-messages"); replay.observe(&partial,None);
        assert_eq!(replay.report()["eligibleProviderUsageRecords"],1); assert_eq!(replay.report()["actualProviderCacheReadRate"],0.8);
    }

    #[test]
    fn explicit_read_write_and_inclusive_input_prices_never_count_tokens_twice() {
        let prices=json!({"model":"fixture","protocol":"openai-responses","currency":"TEST","cacheWriteBilling":"separate",
            "uncachedInputPerMillion":1,"cacheReadPerMillion":0.1,"cacheCreationPerMillion":2,"outputPerMillion":3});
        let usage=json!({"input_tokens":100,"output_tokens":5,"input_tokens_details":{"cached_tokens":70,"cache_write_tokens":20}});
        let result=cost("openai-responses","fixture",&usage,&prices);
        assert_eq!(result["billedOrdinaryInputTokens"],10);
        assert!((result["total"].as_f64().unwrap()-72.0/1_000_000.0).abs()<1e-12);
        let mut inclusive=prices.clone(); inclusive["protocol"]=json!("openai-completions"); inclusive["cacheWriteBilling"]=json!("included-in-input");
        let result=cost("openai-completions","fixture",&json!({"prompt_tokens":100,"completion_tokens":5,"prompt_cache_hit_tokens":80,"prompt_cache_miss_tokens":20}),&inclusive);
        assert_eq!(result["billedOrdinaryInputTokens"],20); assert!(result["reportedCacheCreationTokens"].is_null());
        assert_eq!(cost("openai-completions","other-model",&usage,&prices)["state"],"incomplete");
    }

    #[test]
    fn offline_mutation_advice_prices_creation_without_delaying_required_corrections() {
        let prices=json!({"model":"fixture","protocol":"openai-completions","currency":"TEST","cacheWriteBilling":"separate",
            "uncachedInputPerMillion":1,"cacheReadPerMillion":0.1,"cacheCreationPerMillion":2});
        let mut scenario=json!({"invalidatedCachedTokens":100000,"removableTokens":20000,"remainingRequestsEstimate":100,
            "cacheResidencyAssumption":{"existingPrefixHitFraction":1,"futurePrefixHitFraction":1,"creationFractionOnMiss":1}});
        let advice=mutation_cost_advice("openai-completions","fixture",&scenario,&prices);
        assert_eq!(advice["state"],"estimate"); assert_eq!(advice["costComparison"],"estimated-cheaper-to-rebuild");
        assert!((advice["rebuildAdditionalInputCostEstimate"].as_f64().unwrap()-0.19).abs()<1e-12);
        assert!((advice["futureInputSavingsEstimate"].as_f64().unwrap()-0.2).abs()<1e-12);
        scenario["remainingRequestsEstimate"]=json!(0); scenario["correctnessRequired"]=json!(true);
        let required=mutation_cost_advice("openai-completions","fixture",&scenario,&prices);
        assert_eq!(required["costComparison"],"estimated-cheaper-to-keep-prefix");
        assert_eq!(required["recommendation"],"apply-required-correction-independent-of-cost");
        scenario["cacheResidencyAssumption"]=Value::Null;
        assert_eq!(mutation_cost_advice("openai-completions","fixture",&scenario,&prices)["state"],"incomplete");
    }

    fn function(name: &str) -> Value {
        json!({"type":"function","function":{"name":name,"description":"PRIVATE_SCHEMA","parameters":{"type":"object"}}})
    }

    #[test]
    fn offline_strategy_tools_preserve_selected_set_revocations_and_updated_contracts() {
        let mut strategy=StableToolOrder::default();
        let mut body=json!({"messages":[{"role":"user","content":"PRIVATE_TASK"}],"tools":[function("beta"),function("alpha")]});
        let original=body.clone();
        let first=strategy.project(&body).unwrap();
        assert_eq!(body,original);
        assert_eq!(first["tools"][0]["function"]["name"],"alpha");
        body["tools"]=json!([function("gamma"),function("beta"),function("alpha")]);
        let added=strategy.project(&body).unwrap();
        assert_eq!(added["tools"],json!([function("alpha"),function("beta"),function("gamma")]));
        let mut changed=function("alpha");changed["function"]["parameters"]=json!({"type":"object","required":["newRequiredArgument"]});
        body["tools"]=json!([function("gamma"),changed.clone()]);
        let revoked=strategy.project(&body).unwrap();
        assert_eq!(revoked["tools"],json!([changed,function("gamma")]));
        assert_eq!(revoked["messages"],original["messages"]);
        let unsupported=json!({"tools":[{"type":"web_search_preview"}]});
        assert!(strategy.project(&unsupported).is_err());
    }

    #[test]
    fn offline_strategy_comparison_separates_structural_potential_from_original_actual_accounting() {
        let prices=json!({"protocol":"openai-completions","model":"fixture","currency":"TEST",
            "uncachedInputPerMillion":1,"cacheReadPerMillion":0.1,"outputPerMillion":2,"cacheWriteBilling":"included-in-input"});
        let mut first=row("2026-10-21T10:00:00+08:00",json!({"prompt_tokens":100,"completion_tokens":1,"prompt_cache_hit_tokens":80,"prompt_cache_miss_tokens":20}));
        first["diagnosticEpoch"]=json!("epoch-a");
        first["connection"]=json!({"providerId":"fixture","apiKey":"PRIVATE_KEY"});
        first["request"]["tools"]=json!([function("beta"),function("alpha")]);
        let mut second=first.clone();second["recordedAt"]=json!("2026-10-21T10:00:01+08:00");
        second["request"]["tools"]=json!([function("alpha"),function("beta")]);
        second["request"]["messages"].as_array_mut().unwrap().push(json!({"role":"assistant","content":"PRIVATE_OUTPUT"}));
        second["usage"]=json!({"prompt_tokens":100,"completion_tokens":1,"prompt_cache_hit_tokens":60,"prompt_cache_miss_tokens":40});
        let mut replay=ReplayAnalyzer::default();replay.observe(&first,Some(&prices));replay.observe(&second,Some(&prices));
        let report=replay.report();let observation=&report["observations"][1];
        assert_eq!(observation["prefix"]["breakAt"],"tools");
        assert_eq!(observation["stableToolsStrategy"]["prefix"]["breakAt"],"none");
        assert!(observation["stableToolsStrategy"]["additionalRetainedTokensEstimate"].as_u64().unwrap()>0);
        assert_eq!(report["eligibleProviderUsageRecords"],2);
        assert_eq!(report["cost"]["pricedRecords"],2);
        assert_eq!(report["actualProviderCacheReadRate"],0.7);
        assert!(report["strategyComparison"]["stableSelectedToolOrder"]["actualProviderCacheReadRate"].is_null());
        assert!(report["strategyComparison"]["stableSelectedToolOrder"]["actualCost"].is_null());
        assert_eq!(report["calibration"]["modelProtocolGroups"][0]["pairedOriginalStructureObservations"],1);
        assert!(!report.to_string().contains("PRIVATE_"));
    }

    #[test]
    fn offline_strategy_epochs_and_model_protocol_groups_never_merge_unproven_history() {
        let mut record=row("2026-10-21T10:00:00+08:00",json!({"prompt_tokens":100,"completion_tokens":1,"prompt_cache_hit_tokens":80,"prompt_cache_miss_tokens":20}));
        let mut replay=ReplayAnalyzer::default();
        for (model,epoch) in [("fixture",Some("first")),("fixture",Some("second")),("other",Some("second")),("fixture",None),("fixture",None)] {
            record["model"]=json!(model);record["diagnosticEpoch"]=json!(epoch);replay.observe(&record,None);
        }
        let report=replay.report();
        assert_eq!(report["eligibleProviderUsageRecords"],5);
        assert_eq!(report["actualProviderCacheReadRate"],0.8);
        assert_eq!(report["strategyComparison"]["stableSelectedToolOrder"]["comparisons"],0);
        assert_eq!(report["calibration"]["modelProtocolGroups"].as_array().unwrap().len(),2);
        for observation in report["observations"].as_array().unwrap() { assert_eq!(observation["prefix"]["breakAt"],"baseline"); }
    }

    fn controlled_row(revised: bool) -> Value {
        let mut record=row("2026-10-21T10:00:00+08:00",json!({"prompt_tokens":100,"completion_tokens":1,"prompt_cache_hit_tokens":80,"prompt_cache_miss_tokens":20}));
        record["diagnosticEpoch"]=json!("control-epoch");
        record["request"]["messages"]=json!([
            {"role":"system","content":"PRIVATE_RULES"},
            {"role":"user","content":if revised{"PRIVATE_CONTROL_V2 explicitly supersedes V1"}else{"PRIVATE_CONTROL_V1"}},
            {"role":"user","content":"PRIVATE_TASK"},
            {"role":"assistant","tool_calls":[{"id":"call-a","type":"function","function":{"name":"read","arguments":"{}"}}]},
            {"role":"tool","tool_call_id":"call-a","content":"PRIVATE_RECEIPT"}
        ]);
        record["messageSources"]=json!([
            {"id":"rules","kind":"history"},
            {"id":if revised{"control-v2"}else{"control-v1"},"kind":"control","controlKey":"budget","cacheMutationSafe":true},
            {"id":"user-a","kind":"history"},{"id":"assistant-a","kind":"history"},{"id":"tool-a","kind":"history"}
        ]);
        if revised {record["messageSources"][1]["supersedesId"]=json!("control-v1");}
        record
    }

    #[test]
    fn offline_control_comparison_requires_sources_and_preserves_formal_tool_call_receipt_order() {
        let first=controlled_row(false);let second=controlled_row(true);
        let mut controls=ControlProjection::default();controls.project(&first).unwrap();
        let (append,replace)=controls.project(&second).unwrap();
        assert_eq!(append["messages"].as_array().unwrap().len(),6);
        assert_eq!(append["messages"][3],first["request"]["messages"][3]);
        assert_eq!(append["messages"][4],first["request"]["messages"][4]);
        assert_eq!(append["messages"][5],second["request"]["messages"][1]);
        assert_eq!(replace,second["request"]);
        let mut replay=ReplayAnalyzer::default();replay.observe(&first,None);replay.observe(&second,None);
        let report=replay.report();let compared=&report["observations"][1]["controlStrategies"];
        assert_eq!(compared["appendPrefix"]["breakAt"],"none");
        assert_eq!(compared["replacePrefix"]["breakAt"],"message");
        assert!(!report.to_string().contains("PRIVATE_"));
    }

    #[test]
    fn offline_control_invalidation_and_missing_metadata_reset_without_retaining_revoked_guidance() {
        let first=controlled_row(false);let mut second=controlled_row(true);
        second["messageSources"][1]["immediateInvalidation"]=json!(true);
        let mut replay=ReplayAnalyzer::default();replay.observe(&first,None);replay.observe(&second,None);
        second["messageSources"][1].as_object_mut().unwrap().remove("immediateInvalidation");
        replay.observe(&second,None);
        let report=replay.report();
        assert_eq!(report["observations"][1]["controlStrategies"]["state"],"incomplete");
        assert_eq!(report["observations"][2]["controlStrategies"]["appendPrefix"]["breakAt"],"baseline");
        let state=replay.scopes.values().next().unwrap();
        assert!(state.controls.append_history.iter().all(|(source,_)| source.id!="control-v1"));
        let mut missing=second.clone();missing.as_object_mut().unwrap().remove("messageSources");
        let mut controls=ControlProjection::default();assert!(controls.project(&missing).is_err());
        let mut altered=first.clone();altered["request"]["messages"][2]["content"]=json!("rewritten");
        controls.project(&first).unwrap();assert!(controls.project(&altered).is_err());
    }
}

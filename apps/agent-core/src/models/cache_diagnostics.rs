//! Private, request-local prefix comparisons; provider cache usage remains the source of actual hits.
//! 请求内私有前缀比较；实际缓存命中仍以供应商用量回执为准，不保存第二份请求历史。

use std::io::{self, Write};

use serde::ser::{Serialize, SerializeMap, Serializer};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

const MAXIMUM_MESSAGE_FINGERPRINTS: usize = 8192;

/// One prior digest chain per active loop. Neither the private salt nor digests implement serialization.
/// 每个活动循环仅保存上一份摘要链；私有盐和指纹均不实现序列化，也不进入检查点。
pub struct PrefixDiagnostics {
    salt: [u8; 32],
    previous: Option<Fingerprint>,
}

struct Fingerprint {
    route: [u8; 32],
    system: [u8; 32],
    tools: [u8; 32],
    options: [u8; 32],
    messages: Vec<([u8; 32], u64)>,
    system_tokens: u64,
    tool_tokens: u64,
    total_tokens: u64,
    truncated: bool,
}

/// An uncommitted observation must be discarded when admission or validation prevented dispatch.
/// 准入或校验阻止派发时丢弃待提交观察，不能让未发送的请求污染下一次比较基线。
pub struct PrefixObservation {
    next: Fingerprint,
    report: Value,
}

impl Default for PrefixDiagnostics {
    fn default() -> Self {
        let mut salt = [0; 32];
        salt[..16].copy_from_slice(uuid::Uuid::new_v4().as_bytes());
        salt[16..].copy_from_slice(uuid::Uuid::new_v4().as_bytes());
        Self { salt, previous: None }
    }
}

impl PrefixObservation {
    pub fn report(&self) -> Value { self.report.clone() }
}

struct DigestWriter<'a>(&'a mut Sha256);
impl Write for DigestWriter<'_> {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> { self.0.update(bytes); Ok(bytes.len()) }
    fn flush(&mut self) -> io::Result<()> { Ok(()) }
}

struct Canonical<'a>(&'a Value);
impl Serialize for Canonical<'_> {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        match self.0 {
            Value::Object(fields) => {
                let mut fields = fields.iter().collect::<Vec<_>>();
                fields.sort_by(|left, right| left.0.cmp(right.0));
                let mut map = serializer.serialize_map(Some(fields.len()))?;
                for (key, value) in fields { map.serialize_entry(key, &Canonical(value))?; }
                map.end()
            }
            Value::Array(items) => serializer.collect_seq(items.iter().map(Canonical)),
            value => value.serialize(serializer),
        }
    }
}

fn fingerprint(salt: &[u8; 32], domain: &[u8], previous: &[u8; 32], value: &Value) -> [u8; 32] {
    let mut digest = Sha256::new();
    digest.update(salt); digest.update(domain); digest.update(previous);
    // Serializing JSON values into an infallible hashing writer cannot expose prompt bytes in a receipt.
    // JSON 仅写入无失败的哈希接收器，不复制大图片字符串，也不把提示原文带入回执。
    let _ = serde_json::to_writer(DigestWriter(&mut digest), &Canonical(value));
    digest.finalize().into()
}

fn estimate(value: &Value) -> u64 {
    if value.is_null() { 0 } else { super::protocol::tool_message_tokens(std::slice::from_ref(value), "") }
}

fn system_message(message: &Value) -> bool {
    matches!(message["role"].as_str(), Some("system" | "developer"))
}

impl PrefixDiagnostics {
    pub fn prepare(&self, connection: &Value, model: &str, protocol: &str, body: &Value, scope: &str) -> PrefixObservation {
        let zero = [0; 32];
        let route = fingerprint(&self.salt, b"route", &zero, &json!([
            connection["providerId"], connection["baseUrl"], connection["apiKey"], model, protocol, scope
        ]));
        let messages = body["messages"].as_array().or_else(|| body["input"].as_array());
        let mut chain = zero;
        let mut rows = Vec::with_capacity(messages.map_or(1, |items| items.len().min(MAXIMUM_MESSAGE_FINGERPRINTS)));
        let mut system = json!({"system":body["system"],"instructions":body["instructions"]});
        let mut system_tokens = estimate(&body["system"]).saturating_add(estimate(&body["instructions"]));
        if let Some(first) = messages.and_then(|messages| messages.first()).filter(|message| system_message(message)) {
            system["firstMessage"] = first.clone();
            system_tokens = system_tokens.saturating_add(estimate(first));
        }
        let system = fingerprint(&self.salt, b"system", &zero, &system);
        let tools = fingerprint(&self.salt, b"tools", &zero, &json!({"tools":body["tools"],"functions":body["functions"]}));
        let tool_tokens = if body.get("tools").is_some() || body.get("functions").is_some() {
            super::protocol::estimate_tokens(&json!({"tools":body["tools"],"functions":body["functions"]}).to_string())
        } else { 0 };
        let mut options = serde_json::Map::new();
        if let Some(fields) = body.as_object() {
            for (key, value) in fields {
                if !matches!(key.as_str(), "messages" | "input" | "system" | "instructions" | "tools" | "functions"
                    | "prompt_cache_key" | "prompt_cache_options" | "prompt_cache_retention" | "stream" | "stream_options") {
                    options.insert(key.clone(), value.clone());
                }
            }
        }
        let options = fingerprint(&self.salt, b"options", &zero, &Value::Object(options));
        let mut total_tokens = system_tokens.saturating_add(tool_tokens);
        let mut truncated = false;
        if let Some(messages) = messages {
            for (index, message) in messages.iter().enumerate() {
                let tokens = if index == 0 && system_message(message) { 0 } else { estimate(message) };
                total_tokens = total_tokens.saturating_add(tokens);
                if index < MAXIMUM_MESSAGE_FINGERPRINTS {
                    chain = fingerprint(&self.salt, b"message", &chain, message);
                    rows.push((chain, tokens));
                } else { truncated = true; }
            }
        } else if !body["input"].is_null() {
            let tokens = estimate(&body["input"]);
            chain = fingerprint(&self.salt, b"message", &chain, &body["input"]);
            rows.push((chain, tokens)); total_tokens = total_tokens.saturating_add(tokens);
        }
        let next = Fingerprint { route, system, tools, options, messages: rows, system_tokens, tool_tokens, total_tokens, truncated };
        let mut reasons = Vec::new();
        if !body.is_object() || messages.is_none() && body["input"].is_null() { reasons.push("request-history-unavailable"); }
        if !body["previous_response_id"].is_null() { reasons.push("server-retained-history-not-snapshotted"); }
        if next.truncated || self.previous.as_ref().is_some_and(|previous| previous.truncated) { reasons.push("diagnostic-capacity-only-not-request-limit"); }
        if super::protocol::image_inputs(body) > 0 { reasons.push("image-token-cost-unobserved"); }
        let mut common_messages = 0;
        let mut retained_tokens = 0;
        let mut message_index = None;
        let break_at = if let Some(previous) = &self.previous {
            if previous.route != next.route { "route" }
            else if previous.system != next.system { "system" }
            // Adding tools can invalidate later history too; no claim that only the new schema misses cache.
            // 新增工具也可能使后续历史失效，不能宣称只损失新声明的缓存。
            else if previous.tools != next.tools { "tools" }
            else {
                retained_tokens = next.system_tokens.saturating_add(next.tool_tokens);
                for (left, right) in previous.messages.iter().zip(&next.messages) {
                    if left.0 != right.0 { break; }
                    common_messages += 1; retained_tokens = retained_tokens.saturating_add(right.1);
                }
                if common_messages < previous.messages.len().min(next.messages.len())
                    || next.messages.len() < previous.messages.len() {
                    message_index = Some(common_messages); "message"
                } else { "none" }
            }
        } else { "baseline" };
        let lost = self.previous.as_ref().filter(|_| break_at != "none")
            .map_or(0, |previous| previous.total_tokens.saturating_sub(retained_tokens));
        let options_changed = self.previous.as_ref().is_some_and(|previous| previous.options != next.options);
        let report = json!({"schemaVersion":1,"scope":"main-request","coverage":"one-loop dispatched main requests; auxiliary summaries excluded",
            "state":if !reasons.is_empty(){"incomplete"}else if self.previous.is_none(){"baseline-reset"}else{"compared"},
            "breakAt":break_at,"messageIndex":message_index,"commonMessages":common_messages,"chainLength":next.messages.len(),
            "retainedTokensEstimate":retained_tokens,"lostTokensEstimate":lost,"inputTokensEstimate":next.total_tokens,
            "uncachedCurrentTokensEstimate":next.total_tokens.saturating_sub(retained_tokens),"requestOptionsChanged":options_changed,
            "tokenMethod":"kynxa-structural-estimate-not-provider-tokenizer","comparison":"complete-message boundaries; not provider token LCP",
            "providerCacheUsage":"separate-provider-receipt; never inferred from these estimates","incompleteReasons":reasons,
            "retention":"one-prior-request private digests; process-private epoch; never checkpointed"});
        PrefixObservation { next, report }
    }

    pub fn commit(&mut self, observation: PrefixObservation) { self.previous = Some(observation.next); }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn observe(state: &mut PrefixDiagnostics, body: &Value) -> Value {
        let observation = state.prepare(&json!({"baseUrl":"https://fixture.invalid/v1","apiKey":"SECRET_KEY"}), "fixture-model", "openai-completions", body, "fixture");
        let report = observation.report(); state.commit(observation); report
    }

    #[test]
    fn dispatched_main_prefixes_distinguish_appends_changes_and_new_tools_without_exposing_content() {
        let mut state = PrefixDiagnostics::default();
        let mut body = json!({"messages":[{"role":"system","content":"PRIVATE_SYSTEM"},{"role":"user","content":"PRIVATE_QUERY"}],"tools":[{"name":"read"}]});
        assert_eq!(observe(&mut state,&body)["breakAt"],"baseline");
        body["messages"].as_array_mut().unwrap().push(json!({"role":"assistant","content":"PRIVATE_RESULT"}));
        let append = observe(&mut state,&body);
        assert_eq!(append["breakAt"],"none"); assert_eq!(append["commonMessages"],2);
        body["messages"][1]["content"]=json!("changed"); assert_eq!(observe(&mut state,&body)["messageIndex"],1);
        body["tools"].as_array_mut().unwrap().push(json!({"name":"write"}));
        let tools=observe(&mut state,&body); assert_eq!(tools["breakAt"],"tools"); assert_eq!(tools["retainedTokensEstimate"],0);
        assert!(!tools.to_string().contains("PRIVATE_")); assert!(!tools.to_string().contains("SECRET_KEY"));
    }

    #[test]
    fn canonical_objects_ignore_cache_options_and_uncommitted_requests_never_advance_baseline() {
        let mut state=PrefixDiagnostics::default();
        let body=json!({"messages":[{"role":"user","content":"hello"}],"tools":[{"b":2,"a":1}]});
        let _uncommitted=state.prepare(&Value::Null,"model","openai-completions",&body,"");
        assert_eq!(observe(&mut state,&body)["state"],"baseline-reset");
        let mut changed=body.clone(); changed["prompt_cache_key"]=json!("new-key"); changed["stream"]=json!(true);
        changed["tools"][0]=serde_json::from_str(r#"{"a":1,"b":2}"#).unwrap();
        assert_eq!(observe(&mut state,&changed)["breakAt"],"none");
        assert_eq!(observe(&mut PrefixDiagnostics::default(),&changed)["state"],"baseline-reset");
    }

    #[test]
    fn message_diagnostic_capacity_and_server_history_are_incomplete_without_restricting_request() {
        let mut state=PrefixDiagnostics::default();
        let body=json!({"previous_response_id":"remote-id","input":(0..MAXIMUM_MESSAGE_FINGERPRINTS+1)
            .map(|_|json!({"role":"user","content":"x"})).collect::<Vec<_>>()});
        let report=observe(&mut state,&body); assert_eq!(report["state"],"incomplete");
        assert_eq!(report["chainLength"],MAXIMUM_MESSAGE_FINGERPRINTS);
        assert_eq!(body["input"].as_array().unwrap().len(),MAXIMUM_MESSAGE_FINGERPRINTS+1);
    }
}

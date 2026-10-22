//! Provider-neutral tool-change projection: keep earlier request bytes while the declared tool set grows.
//! 与供应商无关的工具变化投影：工具集合增长时保持此前请求字节不变，按端点能力翻译成原生更新。
//!
//! The loop keeps choosing tools exactly as before. Only the outgoing body changes on a capable route:
//! tools present at the series start stay first, later tools are appended with deferred loading, and an
//! update block at the turn where each tool first appeared activates it. Any mismatch starts a new series
//! and sends the current declarations unchanged, which is the previous behavior.
//! 循环照旧选择工具，只在具备能力的路线上改写发出的请求体：序列开始时的工具保持在前，之后新增的工具以
//! 延迟加载追加，并在其首次出现的轮次插入更新块激活。任一不一致都会重新开始序列并原样发送当前声明，
//! 即此前的行为。

use serde_json::{Value, json};
use sha2::{Digest, Sha256};

/// Required opt-in header value for DeepSeek's mid-conversation tool additions.
/// DeepSeek 中途新增工具所需的 beta 请求头取值。
pub(crate) const DEEPSEEK_TOOL_CHANGES_BETA: &str = "mid-conversation-tool-changes-2026-07-01";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Mode {
    /// Declarations are rebuilt on every change; the request is sent as built.
    /// 每次变化都重建声明，请求按原样发送。
    Rebuild,
    /// New tools activate in history; permission removals require a fresh declaration series.
    /// 新工具在历史中激活；撤权移除必须重新建立声明序列。
    AdditionOnly,
}

fn mode(connection: &Value) -> Mode {
    if connection["protocol"] == "anthropic-messages"
        && connection["routeProjection"]["toolUpdate"] == "addition-only"
    {
        Mode::AdditionOnly
    } else {
        Mode::Rebuild
    }
}

/// Only explicitly supported routes use incremental tool declarations.
/// 仅明确支持的路线使用工具声明增量；普通兼容接口保持原协议行为。
pub(crate) fn supports_additions(connection: &Value) -> bool {
    mode(connection) == Mode::AdditionOnly
}

#[derive(Clone, Debug)]
struct Addition {
    index: usize,
    name: String,
    prefix: String,
}

/// One request series of a loop; it lives only in memory, so a restart simply begins a new series.
/// 一个循环内的请求序列，只保存在内存中；重启后重新开始序列，不影响正式历史。
#[derive(Clone, Debug, Default)]
pub(crate) struct ToolSeries {
    declared: Vec<Value>,
    additions: Vec<Addition>,
    authority: Option<String>,
}

fn digest(messages: &[Value]) -> String {
    format!("{:x}", Sha256::digest(json!(messages).to_string().as_bytes()))
}

fn name(tool: &Value) -> &str {
    tool["name"].as_str().unwrap_or("")
}

impl ToolSeries {
    pub(crate) fn reset(&mut self) {
        *self = Self::default();
    }

    /// Reconcile against the complete current authorized catalog, never a budget-trimmed selected subset.
    /// 以完整的当前授权目录核对，不能用预算裁剪后的短集合替代授权；撤权或定义变化立即重建。
    pub(crate) fn reconcile(&mut self, connection: &Value, authorized: &[Value], authority: &Value) -> Value {
        if mode(connection) != Mode::AdditionOnly {
            self.reset();
            return Value::Null;
        }
        let identity = digest(&[connection["protocol"].clone(), connection["baseUrl"].clone(), authority.clone()]);
        let reason = if self.authority.as_ref().is_some_and(|previous| previous != &identity) {
            Some("authority-changed")
        } else if self.declared.iter().any(|previous| !authorized.iter().any(|tool| name(tool) == name(previous))) {
            Some("tool-removed-or-disabled")
        } else if self.declared.iter().any(|previous| authorized.iter().any(|tool|
            name(tool) == name(previous) && strip_deferred(previous) != strip_deferred(tool))) {
            Some("definition-changed")
        } else { None };
        if reason.is_some() { self.reset(); }
        self.authority = Some(identity);
        reason.map(|reason| json!({"series":"reset","reason":reason})).unwrap_or(Value::Null)
    }

    /// Keep these already declared wire names through routine selection/compaction; hard budget repairs may reset.
    /// 常规选择与压缩保留这些已声明的协议工具名；真实硬预算修复可明确重建序列。
    pub(crate) fn required_tool_names(&self, connection: &Value) -> Vec<String> {
        if mode(connection) != Mode::AdditionOnly { return Vec::new(); }
        self.declared.iter().map(|tool| name(tool).to_owned()).collect()
    }

    fn start(&mut self, tools: &[Value]) {
        self.declared = tools.to_vec();
        self.additions.clear();
    }

    fn break_reason(&self, messages: &[Value], tools: &[Value]) -> &'static str {
        if self.additions.iter().any(|addition| addition.index > messages.len()) {
            return "history-shortened";
        }
        if self.additions.iter().any(|addition| digest(&messages[..addition.index]) != addition.prefix) {
            return "history-rewritten";
        }
        if tools.iter().any(|tool| self.declared.iter().any(|declared| name(declared) == name(tool) && strip_deferred(declared) != *tool)) {
            return "definition-changed";
        }
        "unknown"
    }

    /// Earlier additions stay valid only while the history before each one is byte-identical.
    /// 只有每次新增之前的历史逐字节不变，此前记录的新增才仍然有效。
    fn continues(&self, messages: &[Value], tools: &[Value]) -> bool {
        !self.declared.is_empty()
            && self.additions.iter().all(|addition| {
                addition.index <= messages.len() && digest(&messages[..addition.index]) == addition.prefix
            })
            && tools.iter().all(|tool| {
                self.declared
                    .iter()
                    .find(|declared| name(declared) == name(tool))
                    .is_none_or(|declared| strip_deferred(declared) == *tool)
            })
    }
}

fn strip_deferred(tool: &Value) -> Value {
    let mut tool = tool.clone();
    if let Some(fields) = tool.as_object_mut() {
        fields.remove("defer_loading");
    }
    tool
}

/// Rewrite one built request body for the connection's tool-update capability.
/// 按连接的工具更新能力改写一次已构建的请求体；不具备能力的路线保持原样。
/// Returns a diagnostic of what happened, for the request's cache policy record.
/// 返回本次投影的诊断，写入请求的缓存策略记录。
pub(crate) fn project(connection: &Value, body: &mut Value, series: &mut ToolSeries) -> Value {
    let tools = body["tools"].as_array().cloned().unwrap_or_default();
    if mode(connection) == Mode::Rebuild {
        series.start(&tools);
        return Value::Null;
    }
    // A text-only round (for example a completion review) declares no tools; it neither uses nor ends the series.
    // 纯文字轮次（如完成核对）不声明工具，既不使用也不结束当前序列。
    if tools.is_empty() {
        return Value::Null;
    }
    let messages = body["messages"].as_array().cloned().unwrap_or_default();
    if !series.continues(&messages, &tools) {
        let reason = if series.declared.is_empty() { "first-request" } else { series.break_reason(&messages, &tools) };
        series.start(&tools);
        return json!({"series":"started","reason":reason});
    }
    let added = tools
        .iter()
        .filter(|tool| !series.declared.iter().any(|declared| name(declared) == name(tool)))
        .cloned()
        .collect::<Vec<_>>();
    if !added.is_empty() {
        // The update must follow a user or tool-result turn; trailing runtime system notes may sit in between.
        // 更新块必须位于用户或工具结果轮次之后，中间允许有末尾的运行观测 system 消息；否则按原方式重建声明。
        if messages.iter().rev().find(|message| message["role"] != "system").is_none_or(|message| message["role"] != "user") {
            series.start(&tools);
            return json!({"series":"started","reason":"no-user-turn-before-addition"});
        }
        let prefix = digest(&messages);
        for tool in added {
            series.additions.push(Addition { index: messages.len(), name: name(&tool).to_owned(), prefix: prefix.clone() });
            let mut deferred = tool;
            deferred["defer_loading"] = json!(true);
            series.declared.push(deferred);
        }
    }
    let mut projected = messages;
    let mut indices = series.additions.iter().map(|addition| addition.index).collect::<Vec<_>>();
    indices.dedup();
    for index in indices.into_iter().rev() {
        let blocks = series
            .additions
            .iter()
            .filter(|addition| addition.index == index)
            .map(|addition| json!({"type":"tool_addition","tool":{"type":"tool_reference","name":addition.name}}))
            .collect::<Vec<_>>();
        projected.insert(index, json!({"role":"system","content":blocks}));
    }
    body["messages"] = json!(projected);
    body["tools"] = json!(series.declared);
    json!({"series":"continued","additions":series.additions.len()})
}

/// Extra protocol header required by a projected body, if any.
/// 投影后的请求体所需的额外协议头。
pub(crate) fn beta_header(connection: &Value, body: &Value) -> Option<&'static str> {
    (mode(connection) == Mode::AdditionOnly
        && body["messages"].as_array().into_iter().flatten().any(|message| {
            message["content"].as_array().into_iter().flatten().any(|block| block["type"] == "tool_addition")
        }))
    .then_some(DEEPSEEK_TOOL_CHANGES_BETA)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn connection(tool_update: Value) -> Value {
        json!({"protocol":"anthropic-messages","baseUrl":"https://api.deepseek.com/anthropic",
            "routeProjection":{"kind":"deepseek-messages","toolUpdate":tool_update}})
    }
    fn tool(name: &str) -> Value {
        json!({"name":name,"description":format!("{name} tool"),"input_schema":{"type":"object","properties":{}}})
    }
    fn body(messages: &[Value], tools: &[&str]) -> Value {
        json!({"model":"deepseek-flash","messages":messages,"tools":tools.iter().map(|name| tool(name)).collect::<Vec<_>>()})
    }
    fn user(text: &str) -> Value { json!({"role":"user","content":text}) }
    fn assistant(text: &str) -> Value { json!({"role":"assistant","content":text}) }

    #[test]
    fn additions_keep_every_earlier_byte_and_activate_in_history() {
        let route = connection(json!("addition-only"));
        let mut series = ToolSeries::default();
        let history = vec![user("start")];
        let mut first = body(&history, &["a.read", "c.write"]);
        project(&route, &mut first, &mut series);
        assert_eq!(first, body(&history, &["a.read", "c.write"]), "The first request is sent as built.");
        assert!(beta_header(&route, &first).is_none());

        let history = vec![user("start"), assistant("loading"), user("tool result")];
        let mut second = body(&history, &["a.read", "b.search", "c.write"]);
        project(&route, &mut second, &mut series);
        let tools = second["tools"].as_array().unwrap();
        assert_eq!(tools.iter().map(|tool| name(tool)).collect::<Vec<_>>(), ["a.read", "c.write", "b.search"],
            "Earlier declarations stay first; the new tool is appended.");
        assert_eq!(tools[2]["defer_loading"], true);
        assert_eq!(second["tools"][0], first["tools"][0]);
        let messages = second["messages"].as_array().unwrap();
        assert_eq!(messages.len(), 4);
        assert_eq!(&messages[..3], history.as_slice(), "History before the update is unchanged.");
        assert_eq!(messages[3]["role"], "system");
        assert_eq!(messages[3]["content"][0]["tool"]["name"], "b.search");
        assert_eq!(beta_header(&route, &second), Some(DEEPSEEK_TOOL_CHANGES_BETA));

        let mut later = history.clone();
        later.extend([assistant("searched"), user("more results")]);
        let mut third = body(&later, &["a.read", "b.search", "c.write"]);
        project(&route, &mut third, &mut series);
        assert_eq!(&third["messages"].as_array().unwrap()[..4], messages, "The previous request is an exact prefix.");
        assert_eq!(third["tools"], second["tools"]);
    }

    #[test]
    fn removals_keep_declarations_but_changed_history_or_schemas_start_a_new_series() {
        let route = connection(json!("addition-only"));
        let mut series = ToolSeries::default();
        project(&route, &mut body(&[user("start")], &["a.read", "b.search"]), &mut series);
        let mut trimmed = body(&[user("start"), assistant("x"), user("y")], &["a.read"]);
        project(&route, &mut trimmed, &mut series);
        assert_eq!(trimmed["tools"].as_array().unwrap().len(), 2, "Addition-only routes cannot remove a declared tool.");

        let mut grown = body(&[user("start"), assistant("x"), user("y"), assistant("z"), user("w")], &["a.read", "c.new"]);
        project(&route, &mut grown, &mut series);
        assert!(grown.to_string().contains("tool_addition"));
        // Compaction rewrites the history before the recorded addition, so the series restarts unchanged.
        // 压缩改写了已记录新增之前的历史，序列重新开始，请求按原样发送。
        let compacted = body(&[user("compacted summary"), assistant("x"), user("y")], &["a.read", "c.new"]);
        let mut rewritten = compacted.clone();
        project(&route, &mut rewritten, &mut series);
        assert_eq!(rewritten, compacted);

        let mut changed = body(&[user("compacted summary"), assistant("x"), user("y"), assistant("w"), user("v")], &["a.read", "c.new"]);
        changed["tools"][0]["description"] = json!("changed contract");
        let before = changed.clone();
        project(&route, &mut changed, &mut series);
        assert_eq!(changed, before, "A changed definition restarts the series and sends the body as built.");
    }

    #[test]
    fn runtime_notes_and_text_only_rounds_do_not_break_the_series() {
        let route = connection(json!("addition-only"));
        let mut series = ToolSeries::default();
        project(&route, &mut body(&[user("start")], &["a.read"]), &mut series);
        let note = json!({"role":"system","content":"runtime observation"});
        let history = vec![user("start"), assistant("x"), user("tool result"), note.clone()];
        let mut grown = body(&history, &["a.read", "b.search"]);
        assert_eq!(project(&route, &mut grown, &mut series)["series"], "continued");
        assert_eq!(grown["messages"][4]["content"][0]["type"], "tool_addition",
            "The update follows the trailing runtime note.");
        let mut review = json!({"model":"deepseek-flash","messages":[user("start"), assistant("x"), user("review")]});
        assert!(project(&route, &mut review, &mut series).is_null());
        let mut later = history.clone();
        later.extend([assistant("searched"), user("more")]);
        let mut next = body(&later, &["a.read", "b.search"]);
        assert_eq!(project(&route, &mut next, &mut series)["series"], "continued", "A review round keeps the series.");
        assert_eq!(&next["messages"].as_array().unwrap()[..5], &grown["messages"].as_array().unwrap()[..5]);
    }

    #[test]
    fn authorized_series_survives_soft_pruning_but_revocations_and_upgrades_reset_it() {
        let route = connection(json!("addition-only"));
        let authorized = vec![tool("a.read"),tool("b.write")];
        let mut series = ToolSeries::default();
        series.reconcile(&route,&authorized,&json!({"permission":"work","generation":1}));
        project(&route,&mut body(&[user("start")],&["a.read","b.write"]),&mut series);
        series.reconcile(&route,&authorized,&json!({"permission":"work","generation":1}));
        let mut pruned = body(&[user("start"),assistant("done"),user("next")],&["a.read"]);
        project(&route,&mut pruned,&mut series);
        assert_eq!(pruned["tools"].as_array().unwrap().len(),2);
        assert_eq!(series.required_tool_names(&route),vec!["a.read","b.write"]);
        let removed = series.reconcile(&route,&authorized[..1],&json!({"permission":"work","generation":1}));
        assert_eq!(removed["reason"],"tool-removed-or-disabled");
        assert!(series.required_tool_names(&route).is_empty());
        let mut current = body(&[user("continue")],&["a.read"]);
        project(&route,&mut current,&mut series);
        assert_eq!(current["tools"].as_array().unwrap().len(),1);
        let mut upgraded = authorized[..1].to_vec();
        upgraded[0]["input_schema"]["required"] = json!(["newArgument"]);
        assert_eq!(series.reconcile(&route,&upgraded,&json!({"permission":"work","generation":1}))["reason"],"definition-changed");
        project(&route,&mut body(&[user("upgraded")],&["a.read"]),&mut series);
        assert_eq!(series.reconcile(&route,&authorized,&json!({"permission":"ask","generation":2}))["reason"],"authority-changed");
        assert!(series.required_tool_names(&route).is_empty());
    }

    #[test]
    fn routes_without_the_capability_are_untouched() {
        for route in [connection(Value::Null), json!({"protocol":"openai-completions","baseUrl":"https://api.deepseek.com"})] {
            let mut series = ToolSeries::default();
            project(&route, &mut body(&[user("start")], &["a.read"]), &mut series);
            let mut grown = body(&[user("start"), assistant("x"), user("y")], &["a.read", "b.search"]);
            let before = grown.clone();
            project(&route, &mut grown, &mut series);
            assert_eq!(grown, before);
            assert!(beta_header(&route, &grown).is_none());
        }
    }
}

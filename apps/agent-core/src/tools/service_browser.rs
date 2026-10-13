use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::{Arc, Mutex};

use serde_json::{Value, json};
use url::Url;

use super::intent::{matches, utf16_length};
use super::service_error::{ServiceError, ServiceResult};
use crate::models::protocol::{array, slice, string, truthy};

fn rule(operation: &str, arguments: &Value) -> ServiceResult<Value> {
    let value = super::browser::calculate(operation, arguments)
        .map_err(|code| ServiceError::new(&code, "浏览器策略计算失败。", 409))?;
    if let Some(error) = value.get("error") {
        return Err(ServiceError::new(
            string(&error["code"]),
            string(&error["message"]),
            409,
        ));
    }
    Ok(value)
}
fn identifier(value: &Value) -> Option<String> {
    if value.is_null() {
        None
    } else if let Some(number) = value.as_f64().filter(|_| value.is_number()) {
        // JavaScript String(5.0) is "5"; injected page IDs must retain the observed tab identity.
        // JavaScript 的 String(5.0) 为 "5"；注入的页面 ID 必须保留已观察标签页身份。
        Some(if number.fract() == 0.0 && number.abs() < 1e21 {
            format!("{number:.0}")
        } else {
            value.to_string()
        })
    } else {
        Some(
            value
                .as_str()
                .map(str::to_owned)
                .unwrap_or_else(|| value.to_string()),
        )
    }
}

#[cfg(test)]
mod identifier_tests {
    #[test]
    fn injected_float_page_id_keeps_the_integer_snapshot_identity() {
        assert_eq!(
            super::identifier(&serde_json::json!(5.0)),
            Some("5".to_owned())
        );
        assert_eq!(
            super::identifier(&serde_json::json!(5)),
            Some("5".to_owned())
        );
    }
}
fn merge(value: &mut Value, patch: &Value) {
    if let Some(fields) = patch.as_object() {
        for (key, member) in fields {
            value[key] = member.clone();
        }
    }
}

fn public_url(value: &Value) -> Option<String> {
    let value = value.as_str().filter(|value| utf16_length(value) <= 8192)?;
    let mut url = Url::parse(value).ok()?;
    if !["http", "https", "about"].contains(&url.scheme()) {
        return None;
    }
    let _ = url.set_username("");
    let _ = url.set_password(None);
    url.set_fragment(None);
    let pairs = url
        .query_pairs()
        .map(|(key, value)| (key.into_owned(), value.into_owned()))
        .collect::<Vec<_>>();
    if pairs.iter().any(|(key, _)| {
        matches(
            r"token|secret|password|credential|api[-_]?key|authorization|signature|^code$|^key$",
            key,
        )
    }) {
        url.query_pairs_mut().clear().extend_pairs(pairs.iter().filter(|(key,_)|!matches(r"token|secret|password|credential|api[-_]?key|authorization|signature|^code$|^key$",key)));
        if url.query() == Some("") {
            url.set_query(None);
        }
    }
    Some(url.to_string())
}

#[derive(Default)]
struct BrowserConnection {
    connection_id: String,
    selected: Option<String>,
    generation: u64,
    tabs: HashMap<String, Value>,
    tab_order: VecDeque<String>,
}

struct Preparation {
    arguments: Value,
    browser: Value,
    operation: Value,
    connection: Arc<Mutex<BrowserConnection>>,
    session: Arc<Mutex<Value>>,
    script_check: bool,
}

/// Native browser state binds only observed tabs, snapshots and request-independent session identities.
/// 原生浏览器状态只绑定已观察标签页、快照及独立于请求的会话身份。
#[derive(Default)]
pub struct BrowserState {
    connections: HashMap<String, Arc<Mutex<BrowserConnection>>>,
    sessions: HashMap<String, Arc<Mutex<Value>>>,
    session_order: VecDeque<String>,
    prepared: HashMap<u64, Preparation>,
    drafts: HashMap<u64, Value>,
    sequence: u64,
}

impl BrowserState {
    fn operation(&self, arguments: &Value) -> ServiceResult<Value> {
        rule(
            "tools.browserOperation",
            &json!({"descriptor":arguments["descriptor"],"connection":arguments["connection"],"action":arguments["args"]["action"]}),
        )
    }
    fn state(
        &mut self,
        descriptor: &Value,
        session_id: &Value,
    ) -> (Arc<Mutex<BrowserConnection>>, Arc<Mutex<Value>>) {
        let key = string(&descriptor["key"]);
        let connection = self
            .connections
            .entry(key.into())
            .or_insert_with(|| {
                Arc::new(Mutex::new(BrowserConnection {
                    connection_id: key.into(),
                    ..Default::default()
                }))
            })
            .clone();
        let id = session_id
            .as_str()
            .filter(|id| utf16_length(id) <= 128)
            .unwrap_or(key);
        let session_key = format!("{key}:{id}");
        if !self.sessions.contains_key(&session_key) {
            if self.sessions.len() >= 128 {
                if let Some(old) = self.session_order.pop_front() {
                    self.sessions.remove(&old);
                }
            }
            self.sessions.insert(
                session_key.clone(),
                Arc::new(Mutex::new(
                    json!({"sessionId":id,"connectionId":key,"needsObservation":false}),
                )),
            );
            self.session_order.push_back(session_key.clone());
        }
        (connection, self.sessions[&session_key].clone())
    }
    fn prepare(&mut self, arguments: &Value) -> ServiceResult<Value> {
        let descriptor = &arguments["descriptor"];
        let operation = self.operation(arguments)?;
        if operation.is_null() {
            return Ok(json!({"args":arguments["args"]}));
        }
        let (connection, session) = self.state(descriptor, &arguments["options"]["sessionId"]);
        let mut prepared = arguments["args"].clone();
        let properties = descriptor["originalInputSchema"]["properties"]
            .as_object()
            .map(|fields| {
                fields
                    .iter()
                    .map(|(key, value)| (key.clone(), json!(truthy(value))))
                    .collect::<serde_json::Map<_, _>>()
            })
            .unwrap_or_default();
        let current = connection.lock().unwrap();
        let current_session = session.lock().unwrap();
        let policy = rule(
            "tools.browserPrepareOptions",
            &json!({"operation":operation,"toolName":descriptor["toolName"],"args":prepared,"properties":properties,"selectedTabId":current_session["selectedTabId"],"allowForeground":arguments["options"]["allowForeground"],"background":arguments["options"]["background"]}),
        )?;
        if policy["requiresScriptCheck"] == true && arguments["scriptVerified"] != true {
            let sources = ["function", "code"]
                .iter()
                .filter_map(|field| prepared[*field].as_str())
                .collect::<Vec<_>>();
            drop(current_session);
            drop(current);
            self.sequence += 1;
            let id = self.sequence;
            self.drafts.insert(id, arguments.clone());
            return Ok(json!({"draftId":id,"scriptSources":sources}));
        }
        merge(&mut prepared, &policy["patch"]);
        if policy["injectPageId"] == true {
            prepared["pageId"] = json!(
                string(&current_session["selectedTabId"])
                    .parse::<f64>()
                    .unwrap_or(f64::NAN)
            );
        }
        let selecting = ["select_page", "browser_tabs"].contains(&string(&descriptor["toolName"]));
        let tab = identifier(&prepared["pageId"])
            .or_else(|| {
                if selecting {
                    identifier(&prepared["index"])
                } else {
                    None
                }
            })
            .or_else(|| identifier(&current_session["selectedTabId"]))
            .or_else(|| current.selected.clone());
        let tab_state = tab
            .as_ref()
            .and_then(|id| current.tabs.get(id))
            .unwrap_or(&Value::Null);
        let refs = rule("tools.browserReferences", &json!({"args":prepared}))?;
        rule(
            "tools.browserTargetGuard",
            &json!({"selecting":selecting,"pageIdDeclared":properties.get("pageId"),"sessionTabId":current_session["selectedTabId"],"connectionTabId":current.selected,"tabId":tab,"referenceCount":array(&refs).len(),"hasSnapshot":!tab_state["snapshot"].is_null(),"snapshotSessionId":tab_state["snapshot"]["sessionId"],"sessionId":current_session["sessionId"],"missingReferenceCount":array(&refs).iter().filter(|reference|!array(&tab_state["snapshot"]["refs"]).contains(reference)).count(),"readOnly":operation["readOnly"],"needsObservation":current_session["needsObservation"]}),
        )?;
        let timing = rule(
            "tools.browserTiming",
            &json!({"navigation":operation["navigation"],"timeoutDeclared":properties.get("timeout"),"timeout":prepared["timeout"]}),
        )?;
        merge(&mut prepared, &timing["patch"]);
        let mut browser = json!({"connectionId":descriptor["key"],"serverId":descriptor["serverId"],"engine":operation["engine"],"mode":operation["mode"],"sessionId":current_session["sessionId"],"toolName":descriptor["toolName"],"readOnly":operation["readOnly"],"background":policy["background"],"timeoutMs":timing["timeoutMs"]});
        if let Some(tab) = tab {
            browser["tabId"] = json!(tab);
        }
        if truthy(&tab_state["url"]) {
            browser["url"] = tab_state["url"].clone();
        }
        if let Some(url) = public_url(&prepared["url"]) {
            browser["requestedUrl"] = json!(url);
        }
        drop(current_session);
        drop(current);
        self.sequence += 1;
        let id = self.sequence;
        let script_check = policy["requiresScriptCheck"] == true;
        let result = json!({"args":prepared,"browser":browser,"operation":operation,"preparationId":id,"requiresScriptCheck":script_check});
        self.prepared.insert(
            id,
            Preparation {
                arguments: prepared,
                browser,
                operation,
                connection,
                session,
                script_check,
            },
        );
        Ok(result)
    }
    fn prepared(&self, arguments: &Value) -> ServiceResult<&Preparation> {
        arguments["preparationId"]
            .as_u64()
            .and_then(|id| self.prepared.get(&id))
            .ok_or_else(|| {
                ServiceError::new(
                    "BROWSER_APPROVAL_CHANGED",
                    "浏览器准备身份已释放，请重新发现和准备调用。",
                    409,
                )
            })
    }
    fn approval(&self, prepared: &Preparation) -> ServiceResult<Value> {
        let browser = &prepared.browser;
        let mut result = json!({"connectionId":browser["connectionId"],"sessionId":browser["sessionId"],"toolName":browser["toolName"],"targetsPage":!["new_page","list_pages","browser_tabs"].contains(&string(&browser["toolName"]))});
        if let Some(tab) = browser.get("tabId") {
            result["tabId"] = tab.clone();
        }
        if !array(&rule(
            "tools.browserReferences",
            &json!({"args":prepared.arguments}),
        )?)
        .is_empty()
        {
            let connection = prepared.connection.lock().unwrap();
            if let Some(generation) = connection
                .tabs
                .get(string(&browser["tabId"]))
                .and_then(|tab| tab["snapshot"].get("generation"))
            {
                result["snapshotGeneration"] = generation.clone();
            }
        }
        Ok(result)
    }
    fn observe(&self, arguments: &Value) -> ServiceResult<Value> {
        let prepared = self.prepared(arguments)?;
        let mut connection = prepared.connection.lock().unwrap();
        let mut session = prepared.session.lock().unwrap();
        let text = array(&arguments["content"])
            .iter()
            .filter(|block| block["type"] == "text")
            .map(|block| string(&block["text"]))
            .collect::<Vec<_>>()
            .join("\n");
        let evidence = rule(
            "tools.browserReceiptEvidence",
            &json!({"text":slice(&text,0,256000)}),
        )?;
        let outcome = string(&arguments["outcome"]);
        let mut receipt = prepared.browser.clone();
        receipt["outcome"] = arguments["outcome"].clone();
        receipt["observedAt"] = arguments["observedAt"].clone();
        if outcome == "unknown" {
            session["needsObservation"] = json!(true);
            connection.generation += 1;
        }
        if outcome == "completed" {
            for item in array(&evidence["tabs"]) {
                let Some(url) = public_url(&item["url"]) else {
                    continue;
                };
                let id = string(&item["tabId"]).to_owned();
                if !connection.tabs.contains_key(&id) {
                    if connection.tabs.len() >= 128 {
                        if let Some(old) = connection.tab_order.pop_front() {
                            connection.tabs.remove(&old);
                        }
                    }
                    connection.tab_order.push_back(id.clone());
                }
                let tab = connection
                    .tabs
                    .entry(id.clone())
                    .or_insert_with(|| json!({}));
                tab["url"] = json!(url);
                if item["selected"] == true {
                    connection.selected = Some(id);
                }
            }
            let explicit = identifier(&prepared.arguments["pageId"])
                .or_else(|| {
                    if prepared.browser["toolName"] == "browser_tabs"
                        && prepared.arguments["action"] == "select"
                    {
                        identifier(&prepared.arguments["index"])
                    } else {
                        None
                    }
                })
                .or_else(|| connection.selected.clone());
            let page = public_url(&evidence["pageUrl"]);
            if let Some(id) = explicit {
                session["selectedTabId"] = json!(id);
                let mut tab = connection.tabs.get(&id).cloned().unwrap_or(json!({}));
                if let Some(url) = page {
                    tab["url"] = json!(url);
                }
                if !array(&evidence["refs"]).is_empty()
                    || ["take_snapshot", "browser_snapshot"]
                        .contains(&string(&prepared.browser["toolName"]))
                {
                    connection.generation += 1;
                    tab["snapshot"] = json!({"refs":evidence["refs"],"sessionId":session["sessionId"],"generation":connection.generation});
                    session["needsObservation"] = json!(false);
                }
                if !connection.tabs.contains_key(&id) {
                    connection.tab_order.push_back(id.clone());
                }
                connection.tabs.insert(id.clone(), tab.clone());
                receipt["tabId"] = json!(id);
                if truthy(&tab["url"]) {
                    receipt["url"] = tab["url"].clone();
                }
                if !tab["snapshot"].is_null() {
                    receipt["snapshotGeneration"] = tab["snapshot"]["generation"].clone();
                }
                receipt["targetVerified"] = json!(true);
            } else if let Some(url) = page {
                receipt["url"] = json!(url);
                receipt["targetVerified"] = json!(false);
                receipt["needsTabDiscovery"] = json!(true);
            }
            if prepared.browser["toolName"] == "close_page"
                || prepared.browser["toolName"] == "browser_tabs"
                    && prepared.arguments["action"] == "close"
            {
                let id = identifier(&prepared.browser["tabId"]);
                if let Some(id) = &id {
                    connection.tabs.remove(id);
                    connection.tab_order.retain(|known| known != id);
                }
                if connection.selected == id {
                    connection.selected = None;
                }
                if identifier(&session["selectedTabId"]) == id {
                    session.as_object_mut().unwrap().remove("selectedTabId");
                }
                for known in self.sessions.values() {
                    if Arc::ptr_eq(known, &prepared.session) {
                        continue;
                    }
                    let mut known = known.lock().unwrap();
                    if known["connectionId"] == connection.connection_id
                        && identifier(&known["selectedTabId"]) == id
                    {
                        known.as_object_mut().unwrap().remove("selectedTabId");
                    }
                }
            }
        }
        session["lastReceipt"] = receipt.clone();
        Ok(receipt)
    }
    pub fn apply(&mut self, operation: &str, arguments: &Value) -> ServiceResult<Value> {
        match operation {
            "browser.prepare" => self.prepare(arguments),
            "browser.operation" => self.operation(arguments),
            "browser.release" => {
                if let Some(id) = arguments["preparationId"].as_u64() {
                    self.prepared.remove(&id);
                    self.drafts.remove(&id);
                }
                Ok(Value::Null)
            }
            "browser.scriptCheck" => {
                let id = arguments["preparationId"].as_u64().unwrap_or(0);
                let requires = self.drafts.contains_key(&id)
                    || self
                        .prepared
                        .get(&id)
                        .is_some_and(|prepared| prepared.script_check);
                if requires && arguments["foreground"] == true {
                    return Err(ServiceError::new(
                        "BROWSER_FOREGROUND_FORBIDDEN",
                        "后台任务不能通过脚本激活窗口或打开可能抢焦点的窗口。",
                        409,
                    ));
                }
                Ok(Value::Null)
            }
            "browser.finalize" => {
                let mut draft = self
                    .drafts
                    .remove(&arguments["draftId"].as_u64().unwrap_or(0))
                    .ok_or_else(|| {
                        ServiceError::new(
                            "BROWSER_APPROVAL_CHANGED",
                            "浏览器脚本准备身份已释放。",
                            409,
                        )
                    })?;
                draft["scriptVerified"] = json!(true);
                self.prepare(&draft)
            }
            "browser.scriptSyntax" => Ok(json!(
                super::service_browser_script::requests_foreground(&arguments["syntax"])
            )),
            "browser.approval" => self.approval(self.prepared(arguments)?),
            "browser.verify" => rule(
                "tools.browserApprovalGuard",
                &json!({"expected":arguments["expected"],"current":self.approval(self.prepared(arguments)?)?}),
            ),
            "browser.dispatch" => {
                let prepared = self.prepared(arguments)?;
                if prepared.operation["readOnly"] != true {
                    let mut connection = prepared.connection.lock().unwrap();
                    if let Some(tab) = connection.tabs.get_mut(string(&prepared.browser["tabId"])) {
                        tab.as_object_mut().unwrap().remove("snapshot");
                    }
                }
                Ok(Value::Null)
            }
            "browser.observe" => self.observe(arguments),
            "browser.recovery" => {
                if !["BROWSER_TAB_ID_REQUIRED", "BROWSER_STALE_REFERENCE"]
                    .contains(&string(&arguments["code"]))
                {
                    return Ok(Value::Null);
                }
                let operation = self.operation(arguments)?;
                if operation.is_null() {
                    return Ok(Value::Null);
                }
                let (connection, session) =
                    self.state(&arguments["descriptor"], &arguments["options"]["sessionId"]);
                let connection = connection.lock().unwrap();
                let session = session.lock().unwrap();
                let tab = identifier(&arguments["args"]["pageId"])
                    .or_else(|| identifier(&session["selectedTabId"]));
                if rule(
                    "tools.browserRecoveryAllowed",
                    &json!({"code":arguments["code"],"hasOperation":true,"requestedTabId":tab,"sessionTabId":session["selectedTabId"],"connectionTabId":connection.selected,"pageIdDeclared":truthy(&arguments["descriptor"]["originalInputSchema"]["properties"]["pageId"]),"hasLastUrl":truthy(&session["lastReceipt"]["url"])}),
                )? != true
                {
                    return Ok(Value::Null);
                }
                let url = tab
                    .as_ref()
                    .and_then(|id| connection.tabs.get(id))
                    .and_then(|tab| tab.get("url"))
                    .unwrap_or(&session["lastReceipt"]["url"]);
                let mut result =
                    json!({"sessionId":session["sessionId"],"engine":operation["engine"]});
                if let Some(tab) = tab {
                    result["tabId"] = json!(tab);
                }
                if let Some(selected) = &connection.selected {
                    result["connectionSelectedTabId"] = json!(selected);
                }
                if !url.is_null() {
                    result["url"] = url.clone();
                }
                Ok(result)
            }
            "browser.invalidate" => {
                if let Some(connection) = self.connections.get(string(&arguments["connectionId"])) {
                    let mut connection = connection.lock().unwrap();
                    connection.generation += 1;
                    for tab in connection.tabs.values_mut() {
                        tab.as_object_mut().unwrap().remove("snapshot");
                    }
                }
                Ok(Value::Null)
            }
            "browser.remove" => {
                let id = string(&arguments["connectionId"]);
                self.connections.remove(id);
                let keys = self
                    .sessions
                    .iter()
                    .filter(|(_, session)| session.lock().unwrap()["connectionId"] == id)
                    .map(|(key, _)| key.clone())
                    .collect::<HashSet<_>>();
                self.sessions.retain(|key, _| !keys.contains(key));
                self.session_order.retain(|key| !keys.contains(key));
                Ok(Value::Null)
            }
            "browser.diagnostics" => Ok(json!(
                self.session_order
                    .iter()
                    .filter_map(|key| self.sessions.get(key))
                    .filter_map(|session| {
                        let session = session.lock().unwrap();
                        if truthy(&arguments["sessionId"])
                            && session["sessionId"] != arguments["sessionId"]
                        {
                            return None;
                        }
                        session.get("lastReceipt").cloned()
                    })
                    .collect::<Vec<_>>()
            )),
            "browser.url" => Ok(public_url(&arguments["url"])
                .map(|url| json!(url))
                .unwrap_or(Value::Null)),
            "browser.instructionText" => {
                let text = string(&arguments["text"]);
                let length = utf16_length(text);
                let text = slice(text, length.saturating_sub(8000), length);
                let text = regex::Regex::new(r"(?s)```.*?```")
                    .unwrap()
                    .replace_all(&text, "");
                Ok(json!(
                    regex::Regex::new(r"(?m)^>.*$")
                        .unwrap()
                        .replace_all(&text, "")
                        .into_owned()
                ))
            }
            "browser.close" => {
                self.connections.clear();
                self.sessions.clear();
                self.session_order.clear();
                self.prepared.clear();
                self.drafts.clear();
                Ok(Value::Null)
            }
            _ => Err(ServiceError::new(
                "AGENT_CORE_UNKNOWN_OPERATION",
                "Unknown browser session operation.",
                400,
            )),
        }
    }
}

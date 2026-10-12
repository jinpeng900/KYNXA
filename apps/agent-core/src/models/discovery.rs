use std::collections::HashSet;

use serde_json::{Value, json};

use super::{
    connections::valid_model,
    protocol::{string, truthy},
};

#[derive(Default)]
pub struct ModelDiscoveryState {
    anthropic: bool,
    model_ids: Vec<String>,
    seen_models: HashSet<String>,
    cursors: HashSet<String>,
    pages: usize,
    done: bool,
}

/// Only complete validated pages publish model IDs; unknown pagination never follows credential-bearing URLs.
/// 只有完整校验后的所有页面才发布模型 ID；未知分页不能跟随可能携带凭据的供应商 URL。
impl ModelDiscoveryState {
    pub fn apply(&mut self, operation: &str, args: &Value) -> Result<Value, String> {
        if operation == "models.lifecycle.discovery.init" {
            self.anthropic = args["protocol"] == "anthropic-messages";
            return Ok(
                json!({"anthropic": self.anthropic, "limit": 1000, "maximumPageBytes": 8 * 1024 * 1024, "timeoutMs": 30000}),
            );
        }
        if operation != "models.lifecycle.discovery.page" {
            return Err("AGENT_CORE_UNKNOWN_OPERATION".into());
        }
        if self.done {
            return Err("MODEL_DISCOVERY_CLOSED".into());
        }
        self.pages += 1;
        let result = &args["result"];
        let items = result["data"]
            .as_array()
            .or_else(|| result["models"].as_array());
        let Some(items) = items else {
            return Ok(json!({"error": "模型列表接口返回格式无效。"}));
        };
        for item in items {
            let id = item
                .get("id")
                .filter(|value| !value.is_null())
                .unwrap_or(&item["name"]);
            if !valid_model(id) {
                return Ok(json!({"error": "模型列表包含无效 Model ID，未返回不完整的列表。"}));
            }
            let id = string(id).to_owned();
            if self.seen_models.insert(id.clone()) {
                self.model_ids.push(id);
            }
            if self.model_ids.len() > 10000 {
                return Ok(json!({"error": "模型列表超过 10000 个，未返回不完整的列表。"}));
            }
        }
        if !result["has_more"].is_null() && !result["has_more"].is_boolean()
            || ["next_page_token", "nextPageToken", "next_page"]
                .iter()
                .any(|key| truthy(&result[*key]))
        {
            return Ok(json!({"error": "模型列表返回了不支持的分页格式，未返回不完整的列表。"}));
        }
        if result["has_more"] != true {
            self.done = true;
            return Ok(json!({"done": true, "models": self.model_ids}));
        }
        if !self.anthropic {
            return Ok(json!({"error": "此模型列表的分页协议尚不支持，未返回不完整的列表。"}));
        }
        let cursor = &result["last_id"];
        if items.is_empty() || !valid_model(cursor) || !self.cursors.insert(string(cursor).into()) {
            return Ok(json!({"error": "模型列表分页游标无效或重复，未返回不完整的列表。"}));
        }
        if self.pages >= 100 {
            return Ok(json!({"error": "模型列表分页超过限制，未返回不完整的列表。"}));
        }
        Ok(json!({"done": false, "cursor": cursor}))
    }
}

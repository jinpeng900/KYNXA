use std::collections::{HashMap, HashSet};

use serde_json::{Value, json};

use super::service_error::{ServiceError, ServiceResult};
use super::{configuration, discovery};
use crate::models::protocol::{array, estimate_tokens, string, wire_name};

#[derive(Default)]
pub struct CatalogState {
    descriptors: Vec<Value>,
    declarations: Vec<String>,
    aliases: HashMap<String, usize>,
    selected: Vec<usize>,
    token_budget: u64,
}

impl CatalogState {
    fn load_available(&mut self, arguments: &Value) -> ServiceResult<Value> {
        let descriptors = array(&arguments["descriptors"]);
        let mut available = Vec::new();
        let mut unavailable = Vec::new();
        for name in array(&arguments["names"]) {
            if array(&arguments["heads"]).contains(name) {
                let server_id = string(name).strip_prefix("mcp.").unwrap_or("");
                let tools = descriptors
                    .iter()
                    .filter(|tool| tool["serverId"] == server_id)
                    .collect::<Vec<_>>();
                let before = available.len();
                if tools.is_empty() {
                    unavailable.push(json!({"name":name,"code":arguments["serverErrors"].get(server_id).and_then(Value::as_str).unwrap_or("MCP_NO_ENABLED_TOOLS")}));
                }
                for tool in &tools {
                    if available.contains(&tool["name"]) || tool["webAvailable"] == false {
                        continue;
                    }
                    let mut trial = available.clone();
                    trial.push(tool["name"].clone());
                    let indices = trial
                        .iter()
                        .map(|name| self.aliases.get(string(name)).copied())
                        .collect::<Option<Vec<_>>>();
                    if indices.is_some_and(|indices| self.loadable_selection(&indices).is_some()) {
                        available.push(tool["name"].clone());
                    }
                }
                if !tools.is_empty() && available.len() == before {
                    unavailable.push(json!({"name":name,"code":if tools.iter().all(|tool|tool["webAvailable"]==false){"WEB_STAGE_BUDGET_EXHAUSTED"}else{"TOOL_CATALOG_BUDGET"}}));
                }
                continue;
            }
            let tool = descriptors.iter().find(|tool| tool["name"] == *name);
            let enabled = tool
                .map(|tool| {
                    configuration::calculate(
                        "tools.configuredEnabled",
                        &json!({"config":arguments["currentConfig"],"descriptor":tool}),
                    )
                })
                .transpose()
                .map_err(|code| ServiceError::new(&code, "工具配置验证失败。", 400))?
                .unwrap_or(json!(false))
                == true;
            let changed=tool.map(|tool|configuration::calculate("tools.configurationChanged",&json!({"previous":arguments["previousConfig"],"current":arguments["currentConfig"],"descriptor":tool}))).transpose().map_err(|code|ServiceError::new(&code,"工具配置验证失败。",400))?.unwrap_or(json!(false))==true;
            if tool.is_none() || !enabled {
                let revoked = array(&arguments["revokedNames"]).contains(name) || changed;
                let mut failure = json!({"name":name,"code":if revoked{"AGENT_CONFIG_CHANGED"}else{"TOOL_NOT_FOUND"}});
                if revoked {
                    failure["executed"] = json!(false);
                    failure["recoverable"] = json!(true);
                }
                unavailable.push(failure);
            } else if tool.is_some_and(|tool| tool["webAvailable"] == false) {
                unavailable.push(json!({"name":name,"code":"WEB_STAGE_BUDGET_EXHAUSTED"}));
            } else if !available.contains(name) {
                available.push(name.clone());
            }
        }
        if available.is_empty() {
            let code = [
                "WEB_STAGE_BUDGET_EXHAUSTED",
                "TOOL_CATALOG_BUDGET",
                "AGENT_CONFIG_CHANGED",
            ]
            .into_iter()
            .find(|code| unavailable.iter().any(|failure| failure["code"] == *code))
            .unwrap_or("TOOL_NOT_FOUND");
            return Err(
                ServiceError::new(code, "请求的工具当前不可用，请发现其他能力。", 409)
                    .with_details(if code == "AGENT_CONFIG_CHANGED" {
                        json!({"toolConfigurationRevoked":true})
                    } else {
                        json!({})
                    }),
            );
        }
        let mut result = self.apply("catalog.load", &json!({"names":available}))?;
        if !unavailable.is_empty() {
            result["unavailable"] = json!(unavailable);
        }
        Ok(result)
    }
    fn fits(&self, indices: &[usize]) -> bool {
        if indices.len() > 96 {
            return false;
        }
        let text = format!(
            "[{}]",
            indices
                .iter()
                .map(|index| self.declarations[*index].as_str())
                .collect::<Vec<_>>()
                .join(",")
        );
        estimate_tokens(&text) <= self.token_budget
    }

    fn resolve(&self, names: &[Value]) -> Vec<String> {
        names
            .iter()
            .map(|name| {
                self.aliases
                    .get(string(name))
                    .map(|index| string(&self.descriptors[*index]["name"]).to_owned())
                    .unwrap_or_else(|| string(name).to_owned())
            })
            .collect()
    }

    fn loadable_selection(&self, requested: &[usize]) -> Option<Vec<usize>> {
        // Search/load stay reachable; result paging can be deferred only when it was not explicitly requested.
        // 搜索和加载始终可达；仅在未明确请求结果分页时允许延后分页定义。
        for discovery in [
            &["tool.search", "tool.load", "tool.result.read"][..],
            &["tool.search", "tool.load"][..],
        ] {
            let mut next = self
                .descriptors
                .iter()
                .enumerate()
                .filter(|(_, tool)| discovery.contains(&string(&tool["name"])))
                .map(|(index, _)| index)
                .collect::<Vec<_>>();
            for index in requested {
                if !next.contains(index) {
                    next.push(*index);
                }
            }
            if self.fits(&next) {
                return Some(next);
            }
        }
        None
    }

    pub fn apply(&mut self, operation: &str, arguments: &Value) -> ServiceResult<Value> {
        match operation {
            "catalog.init" => {
                self.descriptors = array(&arguments["descriptors"]).to_vec();
                self.declarations = array(&arguments["declarations"])
                    .iter()
                    .map(|value| string(value).to_owned())
                    .collect();
                if self.descriptors.len() != self.declarations.len() {
                    return Err(ServiceError::new(
                        "INVALID_TOOL_CATALOG",
                        "工具目录投影不完整。",
                        400,
                    ));
                }
                self.aliases.clear();
                for (index, descriptor) in self.descriptors.iter().enumerate() {
                    let name = string(&descriptor["name"]);
                    self.aliases.insert(name.to_owned(), index);
                    self.aliases.insert(wire_name(name), index);
                }
                self.token_budget = arguments["tokenBudget"]
                    .as_f64()
                    .filter(|budget| budget.is_finite())
                    .map(|budget| budget.max(0.0).floor() as u64)
                    .unwrap_or(0);
                let signals = discovery::calculate("tools.selectionSignals", &arguments["options"])
                    .map_err(|code| ServiceError::new(&code, "工具选择线索无效。", 400))?;
                let order = discovery::calculate(
                    "tools.catalogOrder",
                    &json!({"descriptors":self.descriptors,
                    "nameOrder":arguments["nameOrder"],"signals":signals}),
                )
                .map_err(|code| ServiceError::new(&code, "工具排序失败。", 400))?;
                self.selected.clear();
                for index in array(&order)
                    .iter()
                    .filter_map(Value::as_u64)
                    .map(|index| index as usize)
                {
                    let mut next = self.selected.clone();
                    next.push(index);
                    if self.fits(&next) {
                        self.selected.push(index);
                    }
                }
                Ok(json!(self.selected))
            }
            "catalog.resolve" => Ok(json!(self.resolve(array(&arguments["names"])))),
            "catalog.selected" => Ok(json!(self.selected)),
            "catalog.loadAvailable" => self.load_available(arguments),
            "catalog.tokenBudget" => Ok(json!(self.token_budget)),
            "catalog.retain" => match self.apply("catalog.load", arguments) {
                Ok(result) => Ok(result),
                Err(error) if error.code == "TOOL_CATALOG_BUDGET" => Ok(Value::Null),
                Err(error) => Err(error),
            },
            "catalog.fits" | "catalog.canLoad" => {
                let names = array(&arguments["names"]);
                let indices = names
                    .iter()
                    .map(|name| self.aliases.get(string(name)).copied())
                    .collect::<Option<Vec<_>>>();
                let fits = indices.is_some_and(|indices| {
                    if operation == "catalog.fits" {
                        self.fits(&indices)
                    } else {
                        self.loadable_selection(&indices).is_some()
                    }
                });
                Ok(json!(fits))
            }
            "catalog.load" => {
                let mut seen = HashSet::new();
                let resolved = self
                    .resolve(array(&arguments["names"]))
                    .into_iter()
                    .filter(|name| seen.insert(name.clone()))
                    .collect::<Vec<_>>();
                let requested = resolved
                    .iter()
                    .filter_map(|name| self.aliases.get(name).copied())
                    .collect::<Vec<_>>();
                if requested.is_empty() {
                    return Err(ServiceError::new(
                        "TOOL_NOT_FOUND",
                        "工具不存在或已禁用。",
                        404,
                    ));
                }
                let mut next = self.loadable_selection(&requested).ok_or_else(|| {
                    ServiceError::new(
                        "TOOL_CATALOG_BUDGET",
                        "请求的工具定义超过本轮模型预算，请减少选择。",
                        413,
                    )
                })?;
                for index in &self.selected {
                    if next.contains(index) {
                        continue;
                    }
                    let mut additional = next.clone();
                    additional.push(*index);
                    if self.fits(&additional) {
                        next.push(*index);
                    }
                }
                self.selected = next;
                let mut result = json!({"loaded":requested.iter().map(|index| &self.descriptors[*index]["name"]).collect::<Vec<_>>(),
                    "selectedCount":self.selected.len(),"availableCount":self.descriptors.len(),
                    "deferredCount":self.descriptors.len()-self.selected.len()});
                if let Some(index) = self.aliases.get("tool.result.read") {
                    if !self.selected.contains(index) {
                        result["deferredDiscovery"] = json!(["tool.result.read"]);
                    }
                }
                let unavailable = resolved
                    .iter()
                    .filter(|name| !self.aliases.contains_key(*name))
                    .map(|name| json!({"name":name,"code":"TOOL_NOT_FOUND"}))
                    .collect::<Vec<_>>();
                if !unavailable.is_empty() {
                    result["unavailable"] = json!(unavailable);
                }
                Ok(result)
            }
            _ => Err(ServiceError::new(
                "AGENT_CORE_UNKNOWN_OPERATION",
                "Unknown catalog operation.",
                400,
            )),
        }
    }
}

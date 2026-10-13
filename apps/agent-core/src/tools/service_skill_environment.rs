use std::cmp::Ordering;

use serde_json::{Value, json};

use super::intent::{js_trim, matches};
use super::service_error::{ServiceError, ServiceResult};
use crate::models::protocol::{array, string};

pub fn supports_execution(capabilities: &Value) -> bool {
    let proof = &capabilities["skillExecution"];
    proof["manifestVersion"] == 1
        && proof["readOnlyPackage"] == true
        && proof["hashChecked"] == true
}

fn diagnostic(code: &str, message: &str, severity: &str) -> Value {
    json!({"code":code,"message":message,"severity":severity})
}

fn node_version(requirement: &str, current: &str) -> Option<bool> {
    let current = current
        .split('.')
        .map(|part| part.parse::<u64>().ok())
        .collect::<Option<Vec<_>>>()?;
    let terms = requirement.split_whitespace().collect::<Vec<_>>();
    if terms.is_empty() {
        return None;
    }
    let parser = regex::Regex::new(r"^(>=|<=|>|<|=)?(\d+(?:\.\d+){0,2})$").unwrap();
    let mut satisfied = true;
    for term in terms {
        let capture = parser.captures(term)?;
        let operator = capture.get(1).map(|value| value.as_str()).unwrap_or("=");
        let mut expected = capture[2]
            .split('.')
            .map(|part| part.parse::<u64>().ok())
            .collect::<Option<Vec<_>>>()?;
        if operator == "=" {
            satisfied &= expected
                .iter()
                .zip(&current)
                .all(|(expected, current)| expected == current);
            continue;
        }
        let partial = expected.len() < 3;
        if partial && ["<=", ">"].contains(&operator) {
            *expected.last_mut()? += 1;
        }
        while expected.len() < 3 {
            expected.push(0);
        }
        let comparison = current.iter().take(3).cmp(expected.iter());
        satisfied &= match operator {
            ">=" => comparison != Ordering::Less,
            "<=" => {
                if partial {
                    comparison == Ordering::Less
                } else {
                    comparison != Ordering::Greater
                }
            }
            ">" => {
                if partial {
                    comparison != Ordering::Less
                } else {
                    comparison == Ordering::Greater
                }
            }
            _ => comparison == Ordering::Less,
        };
    }
    Some(satisfied)
}

fn runtime(path: &str) -> &'static str {
    let suffix = path.rsplit('.').next().unwrap_or("").to_lowercase();
    match suffix.as_str() {
        "mjs" | "cjs" | "js" => "node",
        "py" => "python",
        "ps1" => "powershell",
        "sh" | "bash" => "bash",
        "cmd" | "bat" => "cmd",
        _ => "unknown",
    }
}

#[derive(Default)]
pub struct SkillEnvironment {
    plan: Value,
    capabilities: Value,
    skill: Value,
    node_version: String,
    script_path: Option<String>,
    requirements: String,
    unverified_compatibility: bool,
    compatibility_reviewed: bool,
}

impl SkillEnvironment {
    fn prepare(&mut self, arguments: &Value) -> ServiceResult<Value> {
        self.skill = arguments["skill"].clone();
        self.capabilities = arguments["capabilities"].clone();
        self.node_version = string(&arguments["nodeVersion"]).into();
        self.script_path = arguments["scriptPath"].as_str().map(str::to_owned);
        let inventory = &arguments["inventory"];
        let capabilities = &self.capabilities;
        let sandbox = supports_execution(capabilities);
        let mut commands = Vec::new();
        for command in array(&capabilities["commands"])
            .iter()
            .filter_map(Value::as_str)
        {
            let command = regex::Regex::new(r"(?i)\.exe$")
                .unwrap()
                .replace(command, "")
                .to_lowercase();
            if !commands.contains(&command) {
                commands.push(command);
            }
        }
        let mut diagnostics = array(&inventory["diagnostics"]).to_vec();
        let mut dependencies = Vec::new();
        let scripts=array(&inventory["files"]).iter().filter(|file|{let path=string(&file["relativePath"]);(path.starts_with("scripts/")||!path.contains('/'))&&matches(r"(?i)\.(?:mjs|cjs|js|py|ps1|sh|bash|cmd|bat|rb|pl|lua|php)$",path)})
            .map(|file|{let path=string(&file["relativePath"]);let runtime=runtime(path);json!({"path":path,"runtime":runtime,"supported":runtime=="node"&&capabilities["available"]==true&&commands.iter().any(|command|command=="node")&&sandbox})}).collect::<Vec<_>>();
        if scripts.iter().any(|script| script["runtime"] == "node") && !sandbox {
            diagnostics.push(diagnostic("SKILL_SANDBOX_UNSUPPORTED","The verified helper does not advertise manifest-v1, hash-checked, read-only skill package execution.","error"));
        }
        for script in &scripts {
            if script["supported"] != true {
                diagnostics.push(diagnostic("SKILL_RUNTIME_UNAVAILABLE",&format!("Script {} requires {}; the verified sandbox does not advertise a supported runtime.",string(&script["path"]),string(&script["runtime"])),if self.script_path.as_deref()==script["path"].as_str(){"error"}else{"warning"}));
            }
        }
        let compatibility = self.skill["metadata"]["compatibility"].as_str();
        self.requirements=regex::Regex::new(r"(?i)(?-u:\b)(?:no|without)\s+(?:extra\s+)?(?:runtime|python|internet|network)(?:\s+access)?\s+(?:is\s+)?required(?-u:\b)").unwrap().replace_all(compatibility.unwrap_or(""),"").into_owned();
        self.compatibility_reviewed = compatibility.is_none_or(str::is_empty);
        self.unverified_compatibility = false;
        if let Some(compatibility) = compatibility.filter(|compatibility| !compatibility.is_empty())
        {
            let parser=regex::Regex::new(r"(?i)^(?:Requires?\s+)?Node(?:\.js)?\s*:?\s*((?:>=|<=|>|<|=)?\d+(?:\.\d+){0,2}(?:\s+(?:>=|<=|>|<|=)?\d+(?:\.\d+){0,2})*)?(?:\s*[.;,]\s*(?:no|without)\s+(?:internet|network)(?:\s+access)?(?:\s+(?:is\s+)?required)?)?\s*\.?$").unwrap();
            let satisfied =
                parser
                    .captures(compatibility)
                    .and_then(|capture| match capture.get(1) {
                        Some(requirement) => node_version(requirement.as_str(), &self.node_version),
                        None => Some(
                            commands.iter().any(|command| command == "node")
                                && capabilities["available"] == true,
                        ),
                    });
            if let Some(satisfied) = satisfied {
                self.compatibility_reviewed = true;
                if !satisfied {
                    diagnostics.push(diagnostic(
                        "SKILL_RUNTIME_VERSION_UNAVAILABLE",
                        "The verified Node runtime does not meet the declared version requirement.",
                        "error",
                    ));
                }
            } else {
                self.unverified_compatibility = matches(
                    r"(?i)(?-u:\b)(?:requires?|required|needs?|must|depends?\s+on)(?-u:\b)|需要|必须|依赖|要求",
                    &self.requirements,
                ) || matches(
                    r"(?i)(?-u:\b)(?:python|ruby|java|php|perl|lua|node(?:\.js)?)\s*(?:>=|<=|>|<|=)?\s*\d",
                    &self.requirements,
                );
                diagnostics.push(diagnostic("SKILL_COMPATIBILITY_REVIEW",if self.unverified_compatibility{"Explicit compatibility requirements need verification before execution."}else{"Compatibility text is descriptive; execution is checked against the selected script runtime and dependency manifests."},"warning"));
            }
        }
        let declared = &self.skill["metadata"]["metadata"];
        for key in ["dependencies", "requires"] {
            if let Some(requirement) = declared[key]
                .as_str()
                .filter(|value| !js_trim(value).is_empty())
            {
                dependencies.push(json!({"ecosystem":"declared","name":key,"requirement":requirement,"source":"SKILL.md","status":"unverified"}));
            }
        }
        let manifests = array(&inventory["files"])
            .iter()
            .filter(|file| {
                let path = string(&file["relativePath"]);
                path == "package.json"
                    || path == "scripts/package.json"
                    || matches(r"(?i)^(?:scripts/)?requirements(?:[-.][^/]*)?\.txt$", path)
            })
            .map(|file| file["relativePath"].clone())
            .collect::<Vec<_>>();
        self.plan = json!({"skillId":self.skill["id"],"compatibility":compatibility,"scripts":scripts,"dependencies":dependencies,"diagnostics":diagnostics,
            "sandbox":{"available":capabilities["available"]==true,"commands":commands,"network":capabilities["network"]==true,"skillExecution":sandbox,"runtimeVersions":{"node":self.node_version}}});
        Ok(json!(manifests))
    }
    fn finish(&mut self, arguments: &Value) -> ServiceResult<Value> {
        let mut diagnostics = array(&self.plan["diagnostics"]).to_vec();
        let mut dependencies = array(&self.plan["dependencies"]).to_vec();
        for manifest in array(&arguments["manifests"]) {
            let resource = &manifest["resource"];
            let source = string(&manifest["path"]);
            if resource["kind"] != "text" || resource["hasMore"] == true {
                diagnostics.push(diagnostic("SKILL_DEPENDENCY_MANIFEST_LIMIT","Dependency manifest is not bounded readable text; requirements need manual review.","error"));
                continue;
            }
            if source.rsplit('/').next() == Some("package.json") {
                let parsed = &manifest["json"];
                let mut valid = manifest["parseFailed"] != true && parsed.is_object();
                if valid {
                    for category in [
                        "dependencies",
                        "optionalDependencies",
                        "peerDependencies",
                        "devDependencies",
                    ] {
                        let required = &parsed[category];
                        if required.is_null() {
                            continue;
                        }
                        if !required.is_object() {
                            valid = false;
                            break;
                        }
                        for pair in array(&manifest["entries"][category]) {
                            if !pair[1].is_string() {
                                valid = false;
                                break;
                            }
                            dependencies.push(json!({"ecosystem":"npm","name":pair[0],"requirement":pair[1],"source":source,"category":category,"status":"unverified"}));
                        }
                        if !valid {
                            break;
                        }
                    }
                }
                if valid && !parsed["engines"]["node"].is_null() {
                    if let Some(requirement) = parsed["engines"]["node"].as_str() {
                        let status = match node_version(requirement, &self.node_version) {
                            Some(true) => "verified",
                            Some(false) => "unavailable",
                            None => "unverified",
                        };
                        dependencies.push(json!({"ecosystem":"runtime","name":"node","requirement":requirement,"source":source,"status":status}));
                    } else {
                        valid = false;
                    }
                }
                if !valid {
                    diagnostics.push(diagnostic("SKILL_DEPENDENCY_MANIFEST_INVALID","Package dependency metadata is invalid; no installation or lifecycle script was attempted.","error"));
                }
            } else {
                for line in string(&resource["content"])
                    .split('\n')
                    .map(js_trim)
                    .filter(|line| !line.is_empty() && !line.starts_with('#'))
                    .take(128)
                {
                    dependencies.push(json!({"ecosystem":"python","name":line,"requirement":line,"source":source,"status":"unverified"}));
                }
            }
        }
        if dependencies
            .iter()
            .any(|dependency| dependency["status"] != "verified")
        {
            diagnostics.push(diagnostic("SKILL_DEPENDENCIES_UNVERIFIED","Declared dependencies have not been verified in the isolated runtime. No package installation or host command was executed.","error"));
        }
        let compatibility = string(&self.plan["compatibility"]);
        if matches(
            r"(?i)(?-u:\b)(?:requires?|required|needs?|must|depends?\s+on)(?-u:\b)|需要|必须|依赖|要求",
            &self.requirements,
        ) && matches(r"(?i)(?:internet|network|https?://)", &self.requirements)
            && !matches(r"(?i)(?:no|without)\s+(?:internet|network)", compatibility)
            && self.capabilities["network"] != true
        {
            diagnostics.push(diagnostic("SKILL_NETWORK_UNAVAILABLE","The skill mentions network requirements; the current verified sandbox has no network access.","error"));
        }
        let errors = diagnostics
            .iter()
            .any(|diagnostic| diagnostic["severity"] == "error");
        let selected = if let Some(path) = &self.script_path {
            array(&self.plan["scripts"])
                .iter()
                .find(|script| script["path"] == *path)
        } else {
            array(&self.plan["scripts"])
                .iter()
                .find(|script| script["supported"] == true)
        };
        self.plan["canRun"] = json!(
            selected.is_some_and(|script| script["supported"] == true)
                && !errors
                && !self.unverified_compatibility
        );
        self.plan["compatible"] = if errors {
            json!(false)
        } else if self.compatibility_reviewed {
            json!(true)
        } else {
            Value::Null
        };
        self.plan["diagnostics"] = json!(diagnostics);
        self.plan["dependencies"] = json!(dependencies);
        Ok(self.plan.clone())
    }
    pub fn apply(&mut self, operation: &str, arguments: &Value) -> ServiceResult<Value> {
        match operation {
            "environment.prepare" => self.prepare(arguments),
            "environment.finish" => self.finish(arguments),
            "environment.supports" => Ok(json!(supports_execution(&arguments["capabilities"]))),
            "environment.verifies" => {
                let result = &arguments["result"];
                let prepared = &arguments["prepared"];
                let script = array(&prepared["files"])
                    .iter()
                    .find(|file| file["relativePath"] == prepared["scriptRelativePath"]);
                Ok(json!(
                    supports_execution(result)
                        && script.is_some()
                        && result["skillExecution"]["script"] == prepared["scriptRelativePath"]
                        && result["skillExecution"]["fileCount"] == array(&prepared["files"]).len()
                        && script
                            .is_some_and(|script| result["skillExecution"]["scriptSha256"]
                                == script["sha256"])
                ))
            }
            "environment.publicInventory" => Ok(
                json!({"files":array(&arguments["inventory"]["files"]).iter().map(|file|json!({"path":file["relativePath"],"size":file["size"],"mimeType":file["mimeType"]})).collect::<Vec<_>>(),
            "totalBytes":arguments["inventory"]["totalBytes"],"truncated":arguments["inventory"]["truncated"],"diagnostics":arguments["inventory"]["diagnostics"]}),
            ),
            _ => Err(ServiceError::new(
                "AGENT_CORE_UNKNOWN_OPERATION",
                "Unknown skill environment operation.",
                400,
            )),
        }
    }
}

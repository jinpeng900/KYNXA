use std::collections::{HashMap, HashSet, VecDeque};
use std::fs;
use std::path::Path;
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};

use serde_json::{Value, json};
use unicode_normalization::UnicodeNormalization;

use super::local_files::{
    absolute_path, inspect, is_link, path_key, path_text, read_checked, sha256, within,
};
use super::service_error::{ServiceError, ServiceResult};
use crate::models::protocol::{array, slice, string};

const MAX_SKILLS: usize = 128;
const MAX_CANDIDATES: usize = 512;

fn cancelled(cancellation: &Option<Arc<AtomicBool>>) -> ServiceResult<()> {
    if cancellation
        .as_ref()
        .is_some_and(|flag| flag.load(Ordering::Relaxed))
    {
        return Err(ServiceError::new(
            "TOOL_CANCELLED",
            "应用技能发现已取消。",
            499,
        ));
    }
    Ok(())
}

pub(crate) fn identity(path: &Path, arguments: &Value) -> Value {
    // Normalize lexical separators before hashing legacy Windows path identities.
    // 对旧 Windows 路径身份求哈希前先统一词法分隔符。
    let normalized_path = absolute_path(&path_text(path));
    let path = normalized_path.as_path();
    let bundled = arguments["bundledDirectory"].as_str().map(absolute_path);
    let official = arguments["officialDirectory"].as_str().map(absolute_path);
    if let (Some(bundled), Some(official)) = (bundled, official) {
        if path_key(&bundled) == path_key(&official) && within(&bundled, path) {
            let relative = path.strip_prefix(&bundled).unwrap_or(path);
            let portable = path_text(relative).replace('\\', "/");
            let id = sha256(format!("kynxa-official-tools/skills/{portable}").as_bytes())[..24]
                .to_owned();
            let mut legacy = Vec::new();
            for base in [
                arguments["legacyDirectory"].as_str().map(absolute_path),
                Some(official),
            ]
            .into_iter()
            .flatten()
            {
                let alias = sha256(path_text(&base.join(relative)).as_bytes())[..24].to_owned();
                if alias != id && !legacy.contains(&alias) {
                    legacy.push(alias);
                }
            }
            return json!({"id":id,"legacyIds":legacy});
        }
    }
    json!({"id":&sha256(path_text(path).as_bytes())[..24],"legacyIds":[]})
}

fn sources(arguments: &Value) -> Vec<Value> {
    let root = absolute_path(string(&arguments["root"]));
    let mut sources = Vec::new();
    if let Some(bundled) = arguments["bundledDirectory"].as_str() {
        sources.push(json!({"path":bundled,"origin":"builtin"}));
    }
    sources.push(json!({"path":path_text(&root.join("Skills")),"origin":"data"}));
    for path in array(&arguments["config"]["skillDirectories"]) {
        sources.push(json!({"path":path,"origin":"configured"}));
    }
    if let Some(workspace) = arguments["context"]["workspaceRoot"]
        .as_str()
        .filter(|path| !path.is_empty())
    {
        sources.push(json!({"path":path_text(&absolute_path(workspace).join(".kynxa").join("skills")),"origin":"workspace"}));
    }
    let mut seen = HashSet::new();
    sources
        .into_iter()
        .enumerate()
        .filter_map(|(priority, mut source)| {
            let path = absolute_path(string(&source["path"]));
            if !seen.insert(path_key(&path)) {
                return None;
            }
            source["path"] = json!(path_text(&path));
            source["priority"] = json!(priority);
            Some(source)
        })
        .collect()
}

fn extension_managed(path: &Path, root: &Path) -> bool {
    if !within(root, path) {
        return false;
    }
    let suffix = path_key(path)
        .strip_prefix(&path_key(root))
        .unwrap_or("")
        .trim_start_matches(std::path::MAIN_SEPARATOR)
        .to_owned();
    if suffix.is_empty() {
        return true;
    }
    let parts = suffix.split(std::path::MAIN_SEPARATOR).collect::<Vec<_>>();
    ["agent", "skills", "mcp", "backups"].contains(&parts[0])
        || parts.len() == 1
            && [
                "extension-migration-info.json",
                "extensions-pointer.previous.json",
                "extension-layout.json",
            ]
            .contains(&parts[0])
}

fn scope(arguments: &Value) -> Value {
    let path = absolute_path(string(&arguments["path"]));
    let lexical = &arguments["scope"];
    let root = absolute_path(string(&lexical["root"]));
    let canonical_root = absolute_path(string(&lexical["canonicalRoot"]));
    let data = absolute_path(string(&lexical["dataRoot"]));
    let canonical_data = absolute_path(string(&lexical["canonicalDataRoot"]));
    let extensions = [&root, &canonical_root];
    let data_roots = [&data, &canonical_data];
    let mut aliases = vec![path.clone()];
    for (lexical, canonical) in [(&root, &canonical_root), (&data, &canonical_data)] {
        for (from, to) in [(canonical, lexical), (lexical, canonical)] {
            if within(from, &path) {
                let suffix = path_text(&path)[path_text(from).len()..]
                    .trim_start_matches(std::path::MAIN_SEPARATOR)
                    .to_owned();
                let alias = to.join(suffix);
                if !aliases.contains(&alias) {
                    aliases.push(alias);
                }
            }
        }
    }
    let bundled = [
        lexical["bundledDirectory"].as_str(),
        lexical["canonicalBundledDirectory"].as_str(),
    ]
    .into_iter()
    .flatten()
    .map(absolute_path);
    let denied = if bundled.into_iter().any(|root| within(&root, &path)) {
        false
    } else if aliases.iter().any(|alias| {
        extensions
            .iter()
            .any(|root| extension_managed(alias, root) && !within(&root.join("Skills"), alias))
    }) {
        true
    } else if extensions
        .iter()
        .any(|root| within(&root.join("Skills"), &path))
        && (!data_roots.iter().any(|root| within(root, &path))
            || extensions
                .iter()
                .any(|extension| data_roots.iter().any(|data| within(data, extension))))
    {
        false
    } else if data_roots.iter().any(|root| {
        if within(&root.join("Skills"), &path) {
            return true;
        }
        let work = root.join("Desktop").join("Projects");
        if !within(&work, &path) {
            return false;
        }
        let suffix = path_text(&path)[path_text(&work).len()..]
            .trim_start_matches(std::path::MAIN_SEPARATOR)
            .to_owned();
        let id = suffix.split(std::path::MAIN_SEPARATOR).next().unwrap_or("");
        regex::Regex::new(r"(?i)^(?:[a-f0-9]{32}|[a-f0-9]{8}-(?:[a-f0-9]{4}-){3}[a-f0-9]{12})$")
            .unwrap()
            .is_match(id)
    }) {
        false
    } else {
        extensions
            .into_iter()
            .chain(data_roots)
            .any(|root| within(root, &path))
    };
    json!({"aliases":aliases.iter().map(|path|path_text(path)).collect::<Vec<_>>(),"denied":denied})
}

/// Discovery owns bounded traversal and source precedence; YAML parsing remains an SDK boundary.
/// 发现服务拥有有界遍历与来源优先级；YAML 解析保留为 SDK 边界。
#[derive(Default)]
pub struct SkillDiscovery {
    arguments: Value,
    sources: VecDeque<Value>,
    current: Option<Value>,
    candidates: VecDeque<String>,
    skills: Vec<Value>,
    identities: HashSet<String>,
    status: Value,
    waiting: Option<String>,
    manifest: Option<(String, Value)>,
}

impl SkillDiscovery {
    fn unavailable(&mut self, file: &str, code: &str, directory: bool) {
        self.status["unavailableCount"] =
            json!(self.status["unavailableCount"].as_u64().unwrap_or(0) + 1);
        let path = absolute_path(file);
        let name_path = if directory {
            path.parent().unwrap_or(&path)
        } else {
            path.parent().unwrap_or(&path)
        };
        let name = name_path
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_default();
        let label = if directory {
            "Application skill directory unavailable"
        } else {
            "Application skill unavailable"
        };
        let mut skill = json!({"id":identity(&path,&self.arguments)["id"],"name":if name.is_empty(){if directory{"Unavailable skill directory".into()}else{"Unavailable application skill".into()}}else{slice(&name,0,100)},
            "description":format!("{label} ({code}); original {} preserved.",if directory{"files"}else{"file"}),"source":path_text(&path),
            "status":"unavailable","standardCompliant":false,"diagnostics":[{"code":code,"message":if directory{"Skill directory unavailable; originals preserved."}else{"Skill unavailable; original file preserved."}}]});
        self.add(&mut skill);
    }

    fn add(&mut self, skill: &mut Value) {
        if skill.is_null() {
            return;
        }
        let path = absolute_path(string(&skill["source"]));
        let canonical = fs::canonicalize(&path).unwrap_or_else(|_| path.clone());
        if !self.identities.insert(path_key(&canonical)) {
            return;
        }
        let source = self.current.as_ref().unwrap_or(&Value::Null);
        let identity = identity(&path, &self.arguments);
        let disabled = array(&self.arguments["config"]["disabledSkills"]);
        skill["origin"] = source["origin"].clone();
        skill["priority"] = source["priority"].clone();
        skill["enabled"] = json!(
            !disabled.contains(&skill["id"])
                && !array(&identity["legacyIds"])
                    .iter()
                    .any(|id| disabled.contains(id))
        );
        self.skills.push(skill.clone());
    }

    fn next(&mut self, arguments: &Value) -> Value {
        if let Some(file) = self.waiting.take() {
            if let Some(error) = arguments.get("error") {
                self.unavailable(&file, string(&error["code"]), false);
            } else {
                self.add(&mut arguments["skill"].clone());
            }
        }
        if self.skills.len() >= MAX_SKILLS {
            if !self.candidates.is_empty() || !self.sources.is_empty() {
                self.status["truncated"] = json!(true);
            }
            return json!({"done":true});
        }
        if let Some(file) = self.candidates.pop_front() {
            self.waiting = Some(file.clone());
            return json!({"load":file});
        }
        if let Some(source) = self.sources.pop_front() {
            self.current = Some(source.clone());
            return json!({"authorize":source["path"]});
        }
        json!({"done":true})
    }

    fn scan(
        &mut self,
        arguments: &Value,
        cancellation: &Option<Arc<AtomicBool>>,
    ) -> ServiceResult<Value> {
        let root = absolute_path(string(
            &self.current.as_ref().unwrap_or(&Value::Null)["path"],
        ));
        if let Some(error) = arguments.get("error") {
            self.unavailable(
                &path_text(&root.join("SKILL.md")),
                string(&error["code"]),
                true,
            );
            return Ok(json!({"candidates":[]}));
        }
        let scan = || -> ServiceResult<(Vec<String>, bool)> {
            let Some(metadata) = inspect(&root, true, false)? else {
                return Ok((Vec::new(), false));
            };
            if !metadata.is_dir() {
                return Err(ServiceError::new(
                    "INVALID_APP_SKILL",
                    "应用技能路径不是目录。",
                    400,
                ));
            }
            let mut candidates = vec![path_text(&root.join("SKILL.md"))];
            let mut truncated = false;
            for (visited, entry) in fs::read_dir(&root).map_err(ServiceError::io)?.enumerate() {
                cancelled(cancellation)?;
                if visited >= MAX_CANDIDATES {
                    truncated = true;
                    break;
                }
                let entry = entry.map_err(ServiceError::io)?;
                let metadata = fs::symlink_metadata(entry.path()).map_err(ServiceError::io)?;
                if metadata.is_dir() && !is_link(&metadata) {
                    candidates.push(path_text(&entry.path().join("SKILL.md")));
                }
            }
            Ok((candidates, truncated))
        };
        match scan() {
            Ok((candidates, truncated)) => {
                // The runtime supplies collation ordinals without owning candidate admission or traversal.
                // 运行时提供排序序号，不拥有候选接纳或遍历职责。
                if truncated {
                    self.status["truncated"] = json!(true);
                }
                self.candidates = candidates.clone().into();
                Ok(json!({"candidates":candidates}))
            }
            Err(error) if error.code == "TOOL_CANCELLED" => Err(error),
            Err(error) => {
                self.unavailable(&path_text(&root.join("SKILL.md")), &error.code, true);
                Ok(json!({"candidates":[]}))
            }
        }
    }

    fn finish(&mut self, include_disabled: bool) -> Value {
        let mut names: HashMap<String, Vec<usize>> = HashMap::new();
        for (index, skill) in self
            .skills
            .iter()
            .enumerate()
            .filter(|(_, skill)| skill["status"] != "unavailable")
        {
            names
                .entry(
                    string(&skill["name"])
                        .nfkc()
                        .collect::<String>()
                        .to_lowercase(),
                )
                .or_default()
                .push(index);
        }
        for conflicts in names.values().filter(|indices| indices.len() > 1) {
            self.status["conflictCount"] =
                json!(self.status["conflictCount"].as_u64().unwrap_or(0) + 1);
            let preferred = *conflicts
                .iter()
                .find(|index| self.skills[**index]["enabled"] == true)
                .unwrap_or(&conflicts[0]);
            let id = self.skills[preferred]["id"].clone();
            let ids = conflicts
                .iter()
                .map(|index| self.skills[*index]["id"].clone())
                .collect::<Vec<_>>();
            for index in conflicts {
                let skill = &mut self.skills[*index];
                skill["conflict"] = json!({"preferredId":id,"preferred":skill["id"]==id,"ids":ids});
                if let Some(diagnostics) = skill["diagnostics"].as_array_mut() {
                    diagnostics.push(json!({"code":"SKILL_NAME_CONFLICT","severity":"warning","message":"Multiple sources use this name; IDs remain distinct and discovery priority is explicit."}));
                }
            }
        }
        self.status["disabledCount"] = json!(
            self.skills
                .iter()
                .filter(|skill| skill["enabled"] != true)
                .count()
        );
        json!({"skills":self.skills.iter().filter(|skill|include_disabled||skill["enabled"]==true).collect::<Vec<_>>(),"status":self.status})
    }

    pub fn apply(
        &mut self,
        operation: &str,
        arguments: &Value,
        cancellation: &Option<Arc<AtomicBool>>,
    ) -> ServiceResult<Value> {
        cancelled(cancellation)?;
        match operation {
            "discovery.officialIdentity" => {
                let path = arguments["relativePath"]
                    .as_str()
                    .unwrap_or("")
                    .replace('\\', "/");
                let parts = path.split('/').collect::<Vec<_>>();
                if path.is_empty()
                    || Path::new(&path).is_absolute()
                    || parts.iter().any(|part| {
                        part.is_empty() || [".", ".."].contains(part) || part.contains([':', '\0'])
                    })
                    || parts.last() != Some(&"SKILL.md")
                {
                    return Err(ServiceError::new(
                        "INVALID_OFFICIAL_SKILL_PATH",
                        "Invalid official skill relative path.",
                        400,
                    ));
                }
                let base = absolute_path(string(&arguments["officialDirectory"]));
                let mut facts = arguments.clone();
                facts["bundledDirectory"] = json!(path_text(&base));
                Ok(identity(&base.join(path), &facts))
            }
            "discovery.manifest" => {
                let path = absolute_path(string(&arguments["path"]));
                let key = path_key(&path);
                if let Some((cached, manifest)) = &self.manifest {
                    if *cached == key {
                        return Ok(manifest.clone());
                    }
                }
                let manifest: Value = serde_json::from_slice(&read_checked(
                    &path,
                    256 * 1024,
                    false,
                    "OFFICIAL_TOOLS_CHANGED",
                )?)
                .map_err(|_| {
                    ServiceError::new(
                        "INVALID_OFFICIAL_TOOLS_MANIFEST",
                        "Invalid official tools package manifest.",
                        400,
                    )
                })?;
                if manifest["schemaVersion"] != 1
                    || manifest["packageId"] != "kynxa-official-tools"
                    || !manifest["version"].is_string()
                    || !["skills", "coreTools", "mcpPresets"]
                        .iter()
                        .all(|field| manifest[*field].is_array())
                {
                    return Err(ServiceError::new(
                        "INVALID_OFFICIAL_TOOLS_MANIFEST",
                        "Invalid official tools package manifest.",
                        400,
                    ));
                }
                self.manifest = Some((key, manifest.clone()));
                Ok(manifest)
            }
            "discovery.normalizedDisabled" => {
                let mut aliases = HashMap::new();
                for skill in array(&arguments["manifest"]["skills"]) {
                    let mut facts = arguments.clone();
                    facts["relativePath"] = skill["path"].clone();
                    let identity =
                        self.apply("discovery.officialIdentity", &facts, cancellation)?;
                    for alias in array(&identity["legacyIds"]) {
                        aliases.insert(string(alias).to_owned(), identity["id"].clone());
                    }
                }
                let mut result = Vec::new();
                for id in array(&arguments["ids"]) {
                    let id = aliases.get(string(id)).unwrap_or(id);
                    if !result.contains(id) {
                        result.push(id.clone());
                    }
                }
                Ok(json!(result))
            }
            "discovery.importPath" => {
                let directory = arguments["directory"]
                    .as_str()
                    .filter(|path| {
                        Path::new(path).is_absolute() && !path.contains(['\0', '\r', '\n'])
                    })
                    .ok_or_else(|| {
                        ServiceError::new(
                            "INVALID_APP_SKILL_IMPORT",
                            "Import requires an absolute skill package directory.",
                            400,
                        )
                    })?;
                Ok(json!(path_text(&absolute_path(directory))))
            }
            "discovery.snapshot" => {
                let skill = &arguments["skill"];
                let inventory = &arguments["inventory"];
                if array(&inventory["files"]).iter().any(|file| {
                    file["relativePath"] == "SKILL.md" && file["sha256"] != skill["sha256"]
                }) {
                    return Err(ServiceError::new(
                        "APP_SKILL_CHANGED",
                        if arguments["purpose"] == "script" {
                            "Skill metadata changed during snapshot preparation."
                        } else {
                            "Skill metadata changed during import validation."
                        },
                        409,
                    ));
                }
                if arguments["purpose"] == "script" {
                    if inventory["valid"] != true {
                        return Err(ServiceError::new(
                            "UNSAFE_SKILL_PACKAGE",
                            "Skill package failed snapshot safety checks.",
                            403,
                        )
                        .with_details(json!({"diagnostics":inventory["diagnostics"]})));
                    }
                    let path = &arguments["path"];
                    let script = array(&inventory["files"])
                        .iter()
                        .find(|file| file["relativePath"] == *path)
                        .ok_or_else(|| {
                            ServiceError::new(
                                "SKILL_RESOURCE_NOT_FOUND",
                                "Skill script was not found in the validated package.",
                                404,
                            )
                        })?;
                    return Ok(script.clone());
                }
                if skill.is_null() {
                    return Err(ServiceError::new(
                        "INVALID_APP_SKILL_IMPORT",
                        "Imported directory must contain SKILL.md.",
                        400,
                    ));
                }
                let diagnostics = array(&skill["diagnostics"])
                    .iter()
                    .chain(array(&inventory["diagnostics"]))
                    .cloned()
                    .collect::<Vec<_>>();
                let valid = skill["standardCompliant"] == true && inventory["valid"] == true;
                if arguments["strict"] == true && !valid {
                    return Err(ServiceError::new(
                        "INVALID_APP_SKILL_IMPORT",
                        "Skill import requires standard metadata and a safe package.",
                        400,
                    )
                    .with_details(json!({"diagnostics":diagnostics})));
                }
                let mut metadata = skill.clone();
                metadata
                    .as_object_mut()
                    .map(|metadata| metadata.remove("content"));
                Ok(
                    json!({"valid":valid,"skill":metadata,"skillRoot":arguments["directory"],"files":inventory["files"],"totalBytes":inventory["totalBytes"],"diagnostics":diagnostics}),
                )
            }
            "discovery.requireNode" => {
                if arguments["runtime"] != "node" {
                    return Err(ServiceError::new(
                        "APP_SKILL_SCRIPT_UNSUPPORTED",
                        "Only Node.js skill scripts are supported by the verified sandbox.",
                        400,
                    ));
                }
                Ok(Value::Null)
            }
            "discovery.importTarget" => Ok(json!(path_text(
                &absolute_path(string(&arguments["root"]))
                    .join("Skills")
                    .join(string(&arguments["name"]).nfkc().collect::<String>())
            ))),
            "discovery.roots" => Ok(json!(
                sources(arguments)
                    .iter()
                    .map(|source| source["path"].clone())
                    .collect::<Vec<_>>()
            )),
            "discovery.identity" => Ok(identity(
                &absolute_path(string(&arguments["path"])),
                arguments,
            )),
            "discovery.canonicalRoots" => {
                let mut result = arguments.clone();
                for (field, target) in [
                    ("root", "canonicalRoot"),
                    ("dataRoot", "canonicalDataRoot"),
                    ("bundledDirectory", "canonicalBundledDirectory"),
                ] {
                    if let Some(path) = arguments[field].as_str() {
                        let path = absolute_path(path);
                        let canonical = match fs::canonicalize(&path) {
                            Ok(path) => path,
                            Err(error) if error.kind() == std::io::ErrorKind::NotFound => path,
                            Err(error) => return Err(ServiceError::io(error)),
                        };
                        result[target] = json!(path_text(&canonical));
                    }
                }
                Ok(result)
            }
            "discovery.scope" => Ok(scope(arguments)),
            "discovery.init" => {
                self.arguments = arguments.clone();
                self.sources = sources(arguments).into();
                self.current = None;
                self.candidates.clear();
                self.skills.clear();
                self.identities.clear();
                self.waiting = None;
                self.status = json!({"maxSkills":MAX_SKILLS,"maxCandidatesPerDirectory":MAX_CANDIDATES,"truncated":false,"unavailableCount":0,"disabledCount":0,"conflictCount":0});
                Ok(self.next(&Value::Null))
            }
            "discovery.scan" => self.scan(arguments, cancellation),
            "discovery.order" => {
                let existing = self.candidates.iter().cloned().collect::<HashSet<_>>();
                let requested = array(&arguments["paths"])
                    .iter()
                    .filter_map(Value::as_str)
                    .map(str::to_owned)
                    .collect::<Vec<_>>();
                if requested.len() != existing.len()
                    || requested.iter().collect::<HashSet<_>>().len() != requested.len()
                    || requested.iter().any(|path| !existing.contains(path))
                {
                    return Err(ServiceError::new(
                        "INVALID_APP_SKILL",
                        "技能候选排序投影不完整。",
                        400,
                    ));
                }
                self.candidates = requested.into();
                Ok(Value::Null)
            }
            "discovery.next" => Ok(self.next(arguments)),
            "discovery.finish" => Ok(self.finish(arguments["includeDisabled"] == true)),
            "discovery.validateId" => {
                let id = string(&arguments["id"]);
                if !regex::Regex::new(r"^[a-f0-9]{24}$").unwrap().is_match(id) {
                    return Err(ServiceError::new(
                        "APP_SKILL_NOT_FOUND",
                        "应用技能 ID 无效。",
                        404,
                    ));
                }
                Ok(Value::Null)
            }
            "discovery.select" => {
                let id = string(&arguments["id"]);
                if !regex::Regex::new(r"^[a-f0-9]{24}$").unwrap().is_match(id) {
                    return Err(ServiceError::new(
                        "APP_SKILL_NOT_FOUND",
                        "应用技能 ID 无效。",
                        404,
                    ));
                }
                let source = array(&arguments["skills"])
                    .iter()
                    .find(|skill| skill["id"] == id)
                    .ok_or_else(|| {
                        ServiceError::new(
                            "APP_SKILL_NOT_FOUND",
                            "应用技能不存在或不属于当前范围。",
                            404,
                        )
                    })?;
                if source["enabled"] != true {
                    return Err(ServiceError::new(
                        "APP_SKILL_DISABLED",
                        "应用技能已禁用。",
                        403,
                    ));
                }
                if source["status"] == "unavailable" {
                    return Err(ServiceError::new(
                        "INVALID_APP_SKILL",
                        string(&source["description"]),
                        400,
                    ));
                }
                Ok(source.clone())
            }
            "discovery.verifyRead" => {
                let source = &arguments["source"];
                let skill = &arguments["skill"];
                if skill.is_null()
                    || skill["id"] != source["id"]
                    || skill["sha256"] != source["sha256"]
                {
                    return Err(ServiceError::new(
                        "APP_SKILL_CHANGED",
                        "应用技能已变化，请重新读取。",
                        409,
                    ));
                }
                let mut result = source.clone();
                if let Some(skill) = skill.as_object() {
                    for (key, value) in skill {
                        result[key] = value.clone();
                    }
                }
                result["diagnostics"] = source["diagnostics"].clone();
                Ok(result)
            }
            _ => Err(ServiceError::new(
                "AGENT_CORE_UNKNOWN_OPERATION",
                "Unknown skill discovery operation.",
                400,
            )),
        }
    }
}

#[cfg(all(test, windows))]
mod official_identity_tests {
    use super::{SkillDiscovery, identity};
    use serde_json::json;
    use std::path::Path;

    #[test]
    fn official_legacy_aliases_match_discovered_windows_path_identities() {
        let facts = json!({"officialDirectory":r"C:\Fixture\official-tools\Skills",
            "bundledDirectory":r"C:\Fixture\official-tools\Skills","legacyDirectory":r"C:\Fixture\skills"});
        let discovered = identity(
            Path::new(r"C:\Fixture\official-tools\Skills\internal-comms\SKILL.md"),
            &facts,
        );
        let mut request = facts;
        request["relativePath"] = json!("internal-comms/SKILL.md");
        let helper = SkillDiscovery::default()
            .apply("discovery.officialIdentity", &request, &None)
            .unwrap();
        assert_eq!(helper, discovered);
        request["relativePath"] = json!(r"internal-comms\SKILL.md");
        assert_eq!(
            SkillDiscovery::default()
                .apply("discovery.officialIdentity", &request, &None)
                .unwrap(),
            helper
        );
    }
}

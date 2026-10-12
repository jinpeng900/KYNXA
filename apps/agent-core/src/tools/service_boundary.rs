use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};

use serde_json::{Value, json};

use super::intent::matches;
use super::local_files::{absolute_path, path_key, path_text, within};
use super::service_error::{ServiceError, ServiceResult};
use crate::models::protocol::{array, string};

pub fn extension_managed(path: &Path, root: &Path) -> bool {
    if !within(root, path) {
        return false;
    }
    let suffix = path_key(path)[path_key(root).len()..]
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

/// Protected identities are published atomically after asynchronous canonical resolution.
/// 异步真实路径解析完成后，原子发布受保护身份快照。
#[derive(Default)]
pub struct StorageBoundary {
    configuration: Value,
    aliases: Vec<(PathBuf, PathBuf)>,
}

impl StorageBoundary {
    fn aliases(&self, path: &Path) -> Vec<PathBuf> {
        let mut result = vec![path.to_owned()];
        let text = path_text(path);
        for (lexical, canonical) in &self.aliases {
            for (from, to) in [(lexical, canonical), (canonical, lexical)] {
                if within(from, path) {
                    let suffix =
                        text[path_text(from).len()..].trim_start_matches(std::path::MAIN_SEPARATOR);
                    let alias = absolute_path(&path_text(&to.join(suffix)));
                    if !result.contains(&alias) {
                        result.push(alias);
                    }
                }
            }
        }
        result
    }
    fn classify(&self, path: &Path) -> Value {
        let root = absolute_path(string(&self.configuration["root"]));
        let data = absolute_path(string(&self.configuration["dataHome"]));
        let extension = absolute_path(string(&self.configuration["extensionRoot"]));
        let official = absolute_path(string(&self.configuration["officialToolsRoot"]));
        let aliases = self.aliases(path);
        let credential = aliases.iter().any(|alias| {
            let filename = alias
                .file_name()
                .map(|name| name.to_string_lossy())
                .unwrap_or_default();
            let connection = matches(r"(?i)^connections(?:[.\-_]|$)", &filename)
                && (within(&data, alias)
                    || [&root, &extension]
                        .iter()
                        .any(|root| within(&root.join("Backups"), alias)));
            let config = matches(r"(?i)^config(?:[.\-_]|$)", &filename)
                && [&root, &extension].iter().any(|root| {
                    [root.join("Agent"), root.join("Backups")]
                        .iter()
                        .any(|root| within(root, alias))
                });
            connection || config || within(&extension.join("Backups").join("Extensions"), alias)
        });
        let owned = aliases.iter().any(|alias| {
            [&root, &data, &extension]
                .iter()
                .any(|root| within(root, alias))
        });
        let managed = aliases
            .iter()
            .any(|alias| extension_managed(alias, &extension));
        let readonly = aliases.iter().any(|alias| {
            within(&official, alias)
                || extension_managed(alias, &extension)
                || array(&self.configuration["controlPaths"])
                    .iter()
                    .any(|control| within(alias, &absolute_path(string(control))))
        });
        let private_result = aliases.iter().any(|alias| {
            within(&root, alias)
                && !within(&root.join("Desktop").join("Projects"), alias)
                && !within(&data.join("Workspaces"), alias)
                && path_key(alias)[path_key(&root).len()..]
                    .split(std::path::MAIN_SEPARATOR)
                    .any(|part| part == "tool-results")
        });
        json!({"credential":credential,"owned":owned,"managedExtension":managed,"readOnlyExtension":readonly,"privateResult":private_result})
    }
    pub fn apply(&mut self, operation: &str, arguments: &Value) -> ServiceResult<Value> {
        match operation {
            "boundary.init" => {
                self.configuration = arguments.clone();
                let mut seen = HashSet::new();
                let mut roots = Vec::new();
                for field in ["root", "dataHome", "extensionRoot", "officialToolsRoot"] {
                    if let Some(path) = arguments[field].as_str() {
                        let path = absolute_path(path);
                        if seen.insert(path_key(&path)) {
                            roots.push(path);
                        }
                    }
                }
                for path in array(&arguments["controlPaths"]) {
                    let path = absolute_path(string(path));
                    if let Some(parent) = path.parent() {
                        if seen.insert(path_key(parent)) {
                            roots.push(parent.to_owned());
                        }
                    }
                }
                self.aliases = roots
                    .iter()
                    .map(|path| (path.clone(), path.clone()))
                    .collect();
                Ok(json!(
                    roots.iter().map(|path| path_text(path)).collect::<Vec<_>>()
                ))
            }
            "boundary.canonical" => {
                let mut aliases = Vec::new();
                for root in array(&arguments["roots"]) {
                    let path = absolute_path(string(root));
                    let canonical = match fs::canonicalize(&path) {
                        Ok(canonical) => canonical,
                        Err(error) if error.kind() == std::io::ErrorKind::NotFound => path.clone(),
                        Err(error) => return Err(ServiceError::io(error)),
                    };
                    aliases.push(json!([path_text(&path), path_text(&canonical)]));
                }
                Ok(json!(aliases))
            }
            "boundary.publish" => {
                self.aliases = array(&arguments["aliases"])
                    .iter()
                    .map(|pair| {
                        (
                            absolute_path(string(&pair[0])),
                            absolute_path(string(&pair[1])),
                        )
                    })
                    .collect();
                Ok(Value::Null)
            }
            "boundary.aliases" => Ok(json!(
                self.aliases(&absolute_path(string(&arguments["path"])))
                    .iter()
                    .map(|path| path_text(path))
                    .collect::<Vec<_>>()
            )),
            "boundary.classify" => Ok(self.classify(&absolute_path(string(&arguments["path"])))),
            _ => Err(ServiceError::new(
                "AGENT_CORE_UNKNOWN_OPERATION",
                "Unknown storage boundary operation.",
                400,
            )),
        }
    }
}

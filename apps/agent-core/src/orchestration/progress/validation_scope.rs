//! Project accepted requirement targets without modifying the original validation ledger.
//! 按正式要求的目标投影核验状态，不改原验证账本。

use std::collections::BTreeSet;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

/// Broker-derived target scopes are projections; receipts remain the sole execution history.
/// 程序推导的目标范围只是投影，执行历史仍只保存在原回执中。
#[derive(Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(in crate::orchestration) struct ValidationScope {
    pub(in crate::orchestration) requirements: Vec<ValidationRequirement>,
    #[serde(default)]
    pub(in crate::orchestration) known_targets: Vec<ValidationTarget>,
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(in crate::orchestration) struct ValidationRequirement {
    pub(in crate::orchestration) key: String,
    pub(in crate::orchestration) targets_known: bool,
    pub(in crate::orchestration) targets: Vec<ValidationTarget>,
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(in crate::orchestration) struct ValidationTarget {
    pub(in crate::orchestration) path_id: String,
    pub(in crate::orchestration) sha256: String,
    #[serde(default)]
    pub(in crate::orchestration) identity: String,
}

impl ValidationScope {
    pub(in crate::orchestration) fn checkpoint(&self) -> Value { json!({"version":1,"scope":self}) }

    fn valid(&self) -> bool {
        !self.requirements.is_empty() && self.requirements.len() <= 128
            && self.requirements.iter().all(|requirement| requirement.targets.len() <= 64
                && (!requirement.targets_known || !requirement.targets.is_empty())
                && is_digest(&requirement.key)
                && requirement.targets.iter().all(valid_target))
            && self.known_targets.len() <= 4096 && self.known_targets.iter().all(valid_target)
    }

    fn complete(&self) -> bool {
        self.valid() && self.requirements.iter().all(|requirement| requirement.targets_known)
    }

    pub(super) fn read_checkpoint(value: &Value) -> Option<Self> {
        if value["version"] != 1 { return None; }
        serde_json::from_value::<Self>(value["scope"].clone()).ok().filter(Self::valid)
    }

    pub(super) fn excludes(&self, receipt: &Value) -> bool {
        if !self.complete() { return false; }
        let Some(mut original) = Self::read_checkpoint(&receipt["acceptanceScope"]) else { return false; };
        // Later real receipts may resolve a still-active target; withdrawn unknown goals stay conservative.
        // 后续真实回执可补齐仍有效目标；已撤销却无归属依据的旧目标不能据此忽略。
        for requirement in &mut original.requirements {
            if !requirement.targets_known {
                let Some(current) = self.requirements.iter().find(|current| current.key == requirement.key) else { return false; };
                *requirement = current.clone();
            }
        }
        let observed = &receipt["codeVersionCoverage"]["after"];
        let Some(files) = observed["files"].as_array().filter(|files| !files.is_empty()) else { return false; };
        if observed["complete"] != true || observed["signature"].as_str().is_none_or(|hash| !is_digest(hash))
            || observed["fileCount"].as_u64() != Some(files.len() as u64)
            || files.iter().any(|file| file["identitySource"].as_str().unwrap_or("pathId") != "pathId"
                || file["pathId"].as_str().is_none_or(|id| !is_digest(id)) || file["sha256"].as_str().is_none_or(|hash| !is_digest(hash))) {
            return false;
        }
        // Native hashes bind versions; normalized identities keep Windows aliases on the same dependency.
        // 原生路径哈希绑定版本，规范身份保证 Windows 路径别名仍属于同一依赖。
        let mut observed_targets = Vec::new();
        for file in files {
            let matching = original.known_targets.iter().chain(&self.known_targets).filter(|target|
                file["pathId"] == target.path_id && file["sha256"] == target.sha256).collect::<Vec<_>>();
            let identities = matching.iter().map(|target| target.identity.as_str()).collect::<BTreeSet<_>>();
            if identities.len() != 1 { return false; }
            observed_targets.push((matching[0].identity.as_str(), matching[0].sha256.as_str()));
        }
        let originally_bound = original.requirements.iter().filter(|requirement|
            requirement.targets.iter().any(|target| observed_targets.iter().any(|(identity, hash)|
                target.identity == *identity && target.sha256 == *hash)))
            .map(|requirement| requirement.key.as_str()).collect::<BTreeSet<_>>();
        !self.requirements.iter().any(|requirement| originally_bound.contains(requirement.key.as_str())
            || requirement.targets.iter().any(|target| observed_targets.iter().any(|(identity, _)| target.identity == *identity)))
    }
}

fn is_digest(value: &str) -> bool {
    value.len() == 64 && value.bytes().all(|byte| byte.is_ascii_hexdigit())
}

fn valid_target(target: &ValidationTarget) -> bool {
    let path = target.identity.trim().replace('\\', "/");
    let normalized = if path.as_bytes().get(1) == Some(&b':') || path.starts_with("//") {
        path.to_lowercase()
    } else { path };
    is_digest(&target.path_id) && is_digest(&target.sha256)
        && !target.identity.is_empty() && target.identity == normalized
}

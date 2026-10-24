//! Match acceptance claims to observed receipt kinds, targets and versions.
//! 按回执类型、目标和版本核对验收引用，不认证语义质量。

use std::collections::{HashMap, HashSet};
use std::sync::OnceLock;

use regex::Regex;
use serde::Serialize;
use serde_json::{Value, json};

use super::{progress::{RunProgress, ValidationRequirement, ValidationScope, ValidationTarget}, task};
use sha2::{Digest, Sha256};

#[derive(Serialize)]
#[serde(rename_all = "kebab-case")]
enum EvidenceKind {
    Execution,
    CommittedOutput,
    ObservedContent,
    ObservedState,
    Visual,
}

/// Required identity fields are constructed from receipts; dynamic target metadata remains protocol data.
/// 必需身份字段取自回执，动态目标元信息仍作为协议数据保留。
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct CompletionProof {
    version: u8,
    id: String,
    references: Vec<Value>,
    tool: String,
    kinds: Vec<EvidenceKind>,
    targets: Vec<Value>,
    visual_hashes: Vec<String>,
    visual_observed: bool,
}

/// One assessment owns one current projection, never another persistent receipt ledger.
/// 同次评审复用一份当前投影，不另建持久回执账本。
pub(super) struct CompletionEvidenceSet {
    pub verification: Value,
    pub valid_references: Vec<Value>,
    pub records: Vec<Value>,
}

impl CompletionEvidenceSet {
    pub(super) fn from_records(verification: Value, valid_references: Vec<Value>, mut records: Vec<Value>) -> Self {
        for proof in &mut records { bind_validation(proof, &verification); }
        Self { verification, valid_references, records: current(&records) }
    }

    pub(super) fn from_observations<'a>(observations: impl Iterator<Item = &'a Value>, verification: Value) -> Self {
        let mut valid_references = Vec::new();
        let mut records = Vec::new();
        for observation in observations.filter(|observation| observation["verifiedSuccess"] == true) {
            if task::is_completion_evidence_tool(observation["name"].as_str().unwrap_or("")) {
                if let Some(id) = observation["id"].as_str().filter(|id| !id.is_empty()) {
                    valid_references.push(json!(id));
                }
                for reference in observation["completionEvidence"].as_array().into_iter().flatten() {
                    if reference.as_str().is_some_and(|value| !value.is_empty()) && !valid_references.contains(reference) {
                        valid_references.push(reference.clone());
                    }
                }
            }
            let proof = &observation["completionProof"];
            if proof.is_object() && proof["id"] == observation["id"] { records.push(proof.clone()); }
        }
        Self::from_records(verification, valid_references, records)
    }

    pub(super) fn decision_input(&self, requirements: &[String], mut input: task::TaskDecisionInput) -> task::TaskDecisionInput {
        input.verification = self.verification.clone();
        input.expected_requirements = requirements.to_vec();
        input.valid_evidence = self.valid_references.clone();
        input.evidence_records = Some(self.records.clone());
        input.requires_readback = requirements.iter().any(|goal| task::requires_result_readback(goal));
        input.requires_visual_readback = requirements.iter().any(|goal| task::requires_visual_observation(goal));
        input.has_readback = has_readback(&self.records, false);
        input.has_visual_readback = has_readback(&self.records, true);
        input
    }

    pub(super) fn validation_scope(&self, requirements: &[String]) -> ValidationScope {
        validation_scope(requirements, &self.records)
    }

    pub(super) fn with_current_requirements(mut self, progress: &RunProgress, requirements: &[String]) -> Self {
        self.verification = progress.verification_for_scope(&self.validation_scope(requirements));
        for proof in &mut self.records { bind_validation(proof, &self.verification); }
        self
    }
}

/// Associate only explicit targets with native receipt identities; unknown dependencies remain active.
/// 仅把明确目标关联到原生回执身份，无法确定的依赖继续参与核验。
pub(super) fn validation_scope(requirements: &[String], records: &[Value]) -> ValidationScope {
    let records = current(records);
    let known_targets = records.iter().flat_map(|record| record["targets"].as_array().into_iter().flatten())
        .filter(|target| target["current"] == true && target["kind"] == "file")
        .filter_map(|target| Some(ValidationTarget {
            path_id:target["nativePathId"].as_str().filter(|id| is_digest(id))?.to_owned(),
            sha256:target["version"].as_str().filter(|hash| is_digest(hash))?.to_ascii_lowercase(),
            identity:normalized("file", target["identity"].as_str()?),
        })).take(4096).collect();
    let requirements = requirements.iter().map(|requirement| {
        let expected = explicit_targets(requirement, &records);
        let mut targets = Vec::new();
        let targets_known = !expected.is_empty() && expected.iter().all(|expected| {
            let matching = records.iter().flat_map(|record| record["targets"].as_array().into_iter().flatten())
                .filter(|target| target["current"] == true && target["kind"] == "file" && target_matches(target, expected))
                .collect::<Vec<_>>();
            let identities = matching.iter().filter_map(|target| target["identity"].as_str()).collect::<HashSet<_>>();
            if identities.len() != 1 || matching.is_empty() { return false; }
            let mut found = false;
            for target in matching {
                let Some(path_id) = target["nativePathId"].as_str().filter(|id| is_digest(id)) else { continue; };
                let Some(version) = target["version"].as_str().filter(|hash| is_digest(hash)) else { continue; };
                if !targets.iter().any(|target: &ValidationTarget| target.path_id == path_id && target.sha256.eq_ignore_ascii_case(version)) {
                    targets.push(ValidationTarget { path_id:path_id.to_owned(), sha256:version.to_ascii_lowercase(),
                        identity:normalized("file", target["identity"].as_str().unwrap_or("")) });
                }
                found = true;
            }
            found
        });
        ValidationRequirement { key:format!("{:x}", Sha256::digest(requirement.as_bytes())), targets_known, targets }
    }).collect();
    ValidationScope { requirements, known_targets }
}

fn is_digest(value: &str) -> bool {
    value.len() == 64 && value.bytes().all(|byte| byte.is_ascii_hexdigit())
}

fn normalized(kind: &str, value: &str) -> String {
    let value = value.trim().replace('\\', "/");
    if kind == "file" && (value.as_bytes().get(1) == Some(&b':') || value.starts_with("//")) {
        value.to_lowercase()
    } else {
        value
    }
}

fn payload(content: &Value) -> &Value {
    content.get("structuredContent").filter(|value| value.is_object())
        .or_else(|| content.get("value").filter(|value| value.is_object()))
        .unwrap_or(content)
}

fn actual_targets(body: &Value) -> Vec<Value> {
    let mut targets = Vec::new();
    let mut sources = vec![(body, body), (&body["source"], body), (&body["locator"], body), (&body["sourceRef"], body)];
    for item in body["items"].as_array().into_iter().flatten().take(1000) {
        sources.extend([(item, item), (&item["locator"], item), (&item["sourceRef"], item)]);
    }
    for (source, owner) in sources {
        for (field, kind) in [("path", "file"), ("appPath", "file"), ("relativePath", "file"), ("url", "url"), ("sourceId", "source"), ("sourceRef", "source-ref")] {
            let Some(identity) = source[field].as_str().filter(|value| !value.trim().is_empty()) else { continue; };
            let native_path_id = (kind == "file").then(|| format!("{:x}", Sha256::digest(identity.as_bytes())));
            let identity = normalized(kind, identity);
            if targets.iter().any(|target: &Value| target["kind"] == kind && target["identity"] == identity) { continue; }
            let version = [source, owner].into_iter().flat_map(|value| ["sha256", "contentHash", "sourceVersion", "sourceRevision"]
                .into_iter().filter_map(move |field| value.get(field)))
                .find(|value| !value.is_null() && *value != "").cloned()
                .or_else(|| owner.get("bindingRevision").filter(|value| !value.is_null()).map(|revision| json!({"bindingRevision":revision})))
                .unwrap_or(Value::Null);
            let start = owner["offset"].as_u64().or(owner["startOffset"].as_u64());
            let end = owner["nextOffset"].as_u64().or(owner["endOffset"].as_u64());
            let total = owner["totalCharacters"].as_u64();
            // Extraction status does not prove that this page contains the whole source.
            // 解析完成不代表本页覆盖全文，已给出的分页范围优先。
            let complete = if start.is_some() || end.is_some() || total.is_some() {
                start == Some(0) && end.is_some() && end == total && owner["hasMore"] != true && owner["truncated"] != true
            } else { (owner["documentComplete"] == true || owner["complete"] == true) && owner["hasMore"] != true && owner["truncated"] != true };
            let mut target = json!({"kind":kind,"identity":identity,"version":version,
                "range":{"start":start,"end":end,"total":total,"complete":complete,
                    "startLine":owner["lineRange"]["startLine"],"endLine":owner["lineRange"]["endLine"],
                    "unit":owner["offsetUnit"].as_str().unwrap_or("utf16-code-units")}});
            if let Some(path_id) = native_path_id { target["nativePathId"] = json!(path_id); }
            targets.push(target);
        }
    }
    targets
}

/// Derive proof from settled output, never from a model-supplied requirement binding.
/// 证明来自已结算输出，不采信模型指定的要求绑定。
pub(super) fn from_receipt(call: &Value, receipt: &Value) -> Value {
    let name = call["name"].as_str().unwrap_or("");
    let Some(id) = call["id"].as_str().filter(|id| !id.is_empty()) else { return Value::Null; };
    if !task::successful_completion_receipt(receipt) || !task::is_completion_evidence_tool(name) {
        return Value::Null;
    }
    let canonical = &receipt["content"];
    let decoded = canonical.as_str().and_then(|text| serde_json::from_str::<Value>(text).ok());
    let body = payload(decoded.as_ref().unwrap_or(canonical));
    let mut kinds = vec![EvidenceKind::Execution];
    let mut references = vec![json!(id)];
    let output_paths = task::completed_output_paths(name, receipt);
    if !output_paths.is_empty() { kinds.push(EvidenceKind::CommittedOutput); }
    for path in output_paths {
        if !references.contains(&path) { references.push(path); }
    }
    if matches!(name, "filesystem.read" | "document.read" | "image.read" | "knowledge.read" | "web.fetch"
        | "web.search" | "knowledge.search" | "browser.read" | "browser.snapshot") {
        kinds.push(EvidenceKind::ObservedContent);
    } else if matches!(name, "filesystem.stat" | "process.list" | "computer.read" | "computer.windows")
        || name.rsplit('.').next().is_some_and(|method| matches!(method,
            "get_scene_info" | "get_object_info" | "list_scene_items" | "get_world_state_snapshot")) {
        kinds.push(EvidenceKind::ObservedState);
    }
    // A terminal exit code alone is not quality proof; the broker adds version-checked validation.
    // 终端退出码不单独证明质量，由调用端补入已核验版本的检查记录。
    let images = task::receipt_visual_hashes(canonical);
    if !images.is_empty() { kinds.push(EvidenceKind::Visual); }
    json!(CompletionProof { version:1, id:id.to_owned(), references, tool:name.to_owned(), kinds,
        targets:actual_targets(body), visual_hashes:images, visual_observed:false })
}

fn version_key(value: &Value) -> Option<String> {
    // Native readers and document checkpoints encode the same content hash in two shapes.
    // 原生读取与文档检查点的同一内容哈希有两种表示，单字段包装不代表文件改版。
    let hash = value.as_str().or_else(|| value.as_object().filter(|fields| fields.len() == 1)
        .and_then(|fields| fields.get("sha256")).and_then(Value::as_str))
        .filter(|hash| hash.len() == 64 && hash.bytes().all(|byte| byte.is_ascii_hexdigit()));
    if let Some(hash) = hash { return Some(json!(hash.to_ascii_lowercase()).to_string()); }
    (!value.is_null() && value != "").then(|| value.to_string())
}

/// Rebind check status to the broker's current aggregate without rewriting execution history.
/// 从程序的当前检查汇总绑定状态，不改写历史执行事实。
pub(super) fn bind_validation(proof: &mut Value, verification: &Value) {
    if !proof.is_object() { return; }
    if let Some(validation) = verification["receipts"].as_array().into_iter().flatten()
        .chain(verification["requiredChecks"].as_array().into_iter().flatten())
        .find(|validation| validation["toolCallId"] == proof["id"]) {
        proof["validation"] = validation.clone();
    } else if proof["validation"].is_object() {
        proof["validation"]["current"] = json!(false);
    }
}

/// Mark stale targets without discarding their historical execution facts.
/// 标记过期目标，保留历史执行事实。
pub(super) fn current(records: &[Value]) -> Vec<Value> {
    let mut latest = HashMap::new();
    let mut changes = HashMap::new();
    let (mut last_mutation, mut last_opaque) = (None, None);
    let mut sequence = 0_u64;
    for (index, record) in records.iter().enumerate() {
        let next_sequence = record["mutationSequence"].as_u64().unwrap_or(sequence);
        let mutating = record["mutating"] == true;
        // A sequence gap represents a change whose receipt scope is unavailable.
        // 修改序号有缺口时，不能假定未保留的回执只改了某个已知目标。
        if next_sequence > sequence.saturating_add(u64::from(mutating)) {
            last_opaque = Some(index);
            last_mutation = Some(index);
        }
        sequence = sequence.max(next_sequence);
        if mutating {
            last_mutation = Some(index);
            if !has_target_scope(record) { last_opaque = Some(index); }
        }
        for target in record["targets"].as_array().into_iter().flatten() {
            let Some(identity) = target["identity"].as_str().filter(|identity| !identity.is_empty()) else {
                if mutating { last_opaque = Some(index); }
                continue;
            };
            let key = (target["kind"].as_str().unwrap_or(""), identity);
            let version = version_key(&target["version"]);
            if let Some(version) = &version { latest.insert(key, version.clone()); }
            if mutating { changes.insert(key, (index, version)); }
        }
    }
    records.iter().enumerate().filter(|(_, record)| record.is_object()).map(|(index, record)| {
        let observation_current = record["observationCurrent"] != false
            && !last_opaque.is_some_and(|changed| changed > index)
            && (has_target_scope(record) || !last_mutation.is_some_and(|changed| changed > index));
        let mut record = record.clone();
        let validation_current = record["validation"]["passed"] == true
            && record["validation"]["coversObservedRevision"] == true
            && record["validation"]["codeVersionCoverage"]["sameObservedVersion"] == true
            && record["validation"]["current"] != false && record["validation"]["active"] != false;
        if let Some(kinds) = record["kinds"].as_array_mut() {
            kinds.retain(|kind| kind != "validation" || validation_current);
            if validation_current && !kinds.contains(&json!("validation")) { kinds.push(json!("validation")); }
        }
        let inherited = record["current"] != false;
        let mut any_current = false;
        if let Some(targets) = record["targets"].as_array_mut() {
            for target in targets {
                let matches = target["identity"].as_str().is_some_and(|identity| {
                    let key = (target["kind"].as_str().unwrap_or(""), identity);
                    let version = version_key(&target["version"]);
                    latest.get(&key).is_none_or(|latest| version.as_ref() == Some(latest))
                        && changes.get(&key).is_none_or(|(changed, changed_version)| *changed <= index
                            || version.is_some() && version == *changed_version)
                });
                target["current"] = json!(inherited && matches);
                any_current |= inherited && matches;
            }
        }
        let no_targets = record["targets"].as_array().is_none_or(Vec::is_empty);
        record["observationCurrent"] = json!(observation_current || validation_current);
        record["current"] = json!(if no_targets {
            inherited
        } else { any_current });
        record
    }).collect()
}

fn has_kind(record: &Value, kind: &str) -> bool {
    record["kinds"].as_array().into_iter().flatten().any(|value| value == kind)
}

fn has_target_scope(record: &Value) -> bool {
    // Image-file hashes identify the container, not the depicted scene or window.
    // 图片文件哈希只标识图片容器，不能冒充画面中的场景或窗口版本。
    record["targets"].as_array().is_some_and(|targets| !targets.is_empty() && targets.iter().all(|target|
        target["identity"].as_str().is_some_and(|identity| !identity.is_empty())
            && (!has_kind(record, "visual") || matches!(target["kind"].as_str(), Some("source" | "source-ref" | "url"))
                && version_key(&target["version"]).is_some())))
}

/// Read the current() projection without cloning it; delivered images survive review rounds.
/// 只读 current() 的有效投影，不重复复制；已投递的有效图片可跨审查轮复用。
pub(super) fn readbacks(records: &[Value], visual: bool) -> impl Iterator<Item = &Value> {
    records.iter().filter(move |record| record["current"] == true && record["observationCurrent"] == true
        && (has_kind(record, "visual") && record["visualObserved"] == true
            || !visual && ["observed-content", "observed-state", "validation"].iter().any(|kind| has_kind(record, kind))))
}

pub(super) fn has_readback(records: &[Value], visual: bool) -> bool {
    readbacks(records, visual).next().is_some()
}

fn annotated_call_reference(reference: &str, id: &str) -> bool {
    // Strip notes only after a complete trusted call ID; paths, URLs and the note cannot supply proof.
    // 仅在可信调用 ID 完整匹配后忽略冒号附注；路径、URL 和附注均不能补造证明。
    if id.is_empty() || id.chars().any(|character| character.is_whitespace() || matches!(character, '/' | '\\' | ':' | '：')) {
        return false;
    }
    let Some(suffix) = reference.strip_prefix(id) else { return false; };
    let suffix = suffix.trim_start();
    let Some(note) = suffix.strip_prefix(':').or_else(|| suffix.strip_prefix('：')) else { return false; };
    let note = note.trim();
    !note.is_empty() && !note.starts_with(['/', '\\'])
}

fn reference_matches(reference: &str, record: &Value, records: &[Value]) -> bool {
    if record["id"] == reference { return true; }
    if record["id"].as_str().is_some_and(|id| annotated_call_reference(reference, id)) { return true; }
    let aliases: Vec<&str> = record["references"].as_array().into_iter().flatten().filter_map(Value::as_str).collect();
    if aliases.iter().any(|alias| *alias == reference || normalized("file", alias) == normalized("file", reference)) { return true; }
    if reference.contains(['/', '\\']) { return false; }
    let paths: HashSet<String> = records.iter().flat_map(|record| record["references"].as_array().into_iter().flatten())
        .filter_map(Value::as_str).filter(|path| path.contains(['/', '\\']))
        .filter(|path| path.replace('\\', "/").rsplit('/').next().is_some_and(|name| name.eq_ignore_ascii_case(reference)))
        .map(|path| normalized("file", path)).collect();
    paths.len() == 1 && aliases.iter().any(|alias| paths.contains(&normalized("file", alias)))
}

fn explicit_targets(requirement: &str, records: &[Value]) -> Vec<String> {
    static TARGET: OnceLock<Regex> = OnceLock::new();
    let regex = TARGET.get_or_init(|| Regex::new(r#"(?iu)https?://[^\s`"<>，。；]+|[^\s`"<>，。；;!?()（）]+\.[a-z][a-z0-9]{0,11}\b"#).expect("static acceptance target expression"));
    let known: Vec<String> = records.iter().flat_map(|record| record["targets"].as_array().into_iter().flatten())
        .filter(|target| matches!(target["kind"].as_str(), Some("file" | "url")))
        .filter_map(|target| target["identity"].as_str()).map(|value| value.replace('\\', "/")).collect();
    regex.find_iter(requirement).map(|found| {
        let text = found.as_str().trim_matches(['\'', ':', '：']).replace('\\', "/");
        let candidates: Vec<&str> = known.iter().filter_map(|path| {
            if text.contains(path.as_str()) { return Some(path.as_str()); }
            let name = path.rsplit('/').next()?;
            text.match_indices(name).any(|(start, _)| {
                let boundary = |ch: char| !ch.is_ascii_alphanumeric() && !matches!(ch, '_' | '-' | '.');
                text[..start].chars().next_back().is_none_or(boundary)
                    && text[start + name.len()..].chars().next().is_none_or(boundary)
            }).then_some(name)
        }).collect();
        if candidates.len() == 1 { return candidates[0].to_owned(); }
        // An adjacent Chinese verb is request text, not part of an observed filename.
        // 紧邻文件名的中文动词属于请求文字，不属于实际文件名。
        for verb in ["保存", "生成", "创建", "读取", "检查", "核对", "验证", "打开", "导出"] {
            if let Some(rest) = text.strip_prefix(verb) {
                if known.iter().any(|path| path.rsplit('/').next() == Some(rest)) { return rest.to_owned(); }
            }
        }
        text
    }).collect()
}

fn target_matches(target: &Value, expected: &str) -> bool {
    let Some(actual) = target["identity"].as_str() else { return false; };
    let kind = target["kind"].as_str().unwrap_or("");
    let expected = if kind == "file" && (actual.as_bytes().get(1) == Some(&b':') || actual.starts_with("//")) {
        expected.replace('\\', "/").to_lowercase()
    } else { normalized(kind, expected) };
    let actual = normalized(kind, actual);
    actual == expected || kind == "file" && !expected.contains('/') && actual.rsplit('/').next() == Some(expected.as_str())
}

fn complete_ranges(targets: &[&Value]) -> bool {
    if targets.iter().any(|target| target["range"]["complete"] == true) { return true; }
    let mut groups: HashMap<String, Vec<(u64, u64, u64)>> = HashMap::new();
    for target in targets {
        if let (Some(start), Some(end), Some(total)) = (target["range"]["start"].as_u64(), target["range"]["end"].as_u64(), target["range"]["total"].as_u64()) {
            let identity = json!([target["kind"], target["identity"], target["version"], target["range"]["unit"]]).to_string();
            groups.entry(identity).or_default().push((start, end, total));
        }
    }
    groups.values_mut().any(|ranges| {
        ranges.sort_unstable();
        let Some((_, _, total)) = ranges.first().copied() else { return false; };
        let mut end = 0;
        for &(start, next, candidate_total) in ranges.iter() {
            if candidate_total != total || start > end || next < start { return false; }
            end = end.max(next);
        }
        end == total
    })
}

fn acceptance_clauses(requirement: &str) -> Vec<&str> {
    static SEPARATOR: OnceLock<Regex> = OnceLock::new();
    static ACTION: OnceLock<Regex> = OnceLock::new();
    let separator = SEPARATOR.get_or_init(|| Regex::new(r"并且?|然后|随后|\s+(?:and|then)\s+|[,，;；]")
        .expect("static acceptance clauses expression"));
    let action = ACTION.get_or_init(|| Regex::new(concat!(
        r"(?i)^\s*(?:(?:请|先|再|随后|然后|最后|同时)\s*|(?:please|then|also|finally)\s+)*",
        r"(?:读取|阅读|回读|读回|写入|写\s|创建|生成|保存|导出|修改|删除|检查|验证|核对|测试|渲染|打开|启动|记录|回复|交付",
        r"|(?:read|write|create|generate|save|export|modify|delete|inspect|check|verify|validate|test|render|open|launch|record|reply|deliver)\b)"
    )).expect("static acceptance action expression"));
    let mut quoted = Vec::new();
    let mut quote = None;
    let mut escaped = false;
    for (offset, character) in requirement.char_indices() {
        if escaped { escaped = false; continue; }
        if character == '\\' { escaped = true; continue; }
        if let Some((start, end)) = quote {
            if character == end { quoted.push((start, offset + character.len_utf8())); quote = None; }
        } else {
            let end = match character { '"' => Some('"'), '`' => Some('`'), '“' => Some('”'),
                '「' => Some('」'), '《' => Some('》'), '\'' if requirement[..offset].chars().next_back()
                    .is_none_or(|previous| !previous.is_alphanumeric()) => Some('\''), _ => None };
            if let Some(end) = end { quote = Some((offset,end)); }
        }
    }
    if let Some((start,_)) = quote { quoted.push((start,requirement.len())); }
    let mut clauses = Vec::new();
    let mut start = 0;
    for found in separator.find_iter(requirement) {
        if quoted.iter().any(|&(start,end)| found.start() >= start && found.start() < end) { continue; }
        let punctuation = found.as_str().chars().next().is_some_and(|ch| matches!(ch, ','|'，'|';'|'；'));
        if punctuation {
            if !action.is_match(&requirement[found.end()..]) { continue; }
            let token = requirement[..found.start()].split_whitespace().next_back().unwrap_or("");
            let token = token.to_ascii_lowercase();
            let in_url = token.contains("http://") || token.contains("https://");
            if in_url && requirement[found.end()..].chars().next().is_none_or(|ch| !ch.is_whitespace()) { continue; }
        }
        // Split an explicit next action without carrying its source targets into the prior write/read check.
        // 明确的下一动作单独核验，不能把前一读取目标误当成后一写入目标；引号及 URL 内标点保留。
        if !requirement[start..found.start()].trim().is_empty() { clauses.push(&requirement[start..found.start()]); }
        start = found.end();
    }
    if !requirement[start..].trim().is_empty() { clauses.push(&requirement[start..]); }
    clauses
}

/// Check the referenced receipt's coverage; model labels cannot change its target or proof kind.
/// 核对引用回执的覆盖，模型标签不能改变其目标或证明类型。
pub(super) fn supports_claim(claim: &Value, requirement: &str, records: &[Value]) -> bool {
    let clauses = acceptance_clauses(requirement);
    if clauses.len() > 1 {
        return clauses.iter().all(|part| supports_claim(claim, part, records));
    }
    // Deciding whether to act is not an unconditional instruction to perform that action.
    // 判断是否执行不等于必须执行，不能为通过验收而补做已撤销或不需要的副作用。
    static OPTIONAL_ACTION: OnceLock<Regex> = OnceLock::new();
    let decision = OPTIONAL_ACTION.get_or_init(|| Regex::new(concat!(
        r"(?i)(?:决定|判断|评估)\s*(?:是否|要不要)\s*(?:需要|应当|应该|要|可以)?\s*",
        r"(?:写入|写|修改|删除|创建|生成|保存|导出|启动|打开|读取|渲染)",
        r"|\b(?:decide|determine|assess)\s+whether\s+(?:(?:or\s+not|we\s+should)\s+)?(?:to\s+)?",
        r"(?:write|modify|delete|create|generate|save|export|launch|open|read|render)\b"
    )).expect("static conditional action expression")).find(requirement);
    let requirement = decision.map_or(requirement, |decision| &requirement[..decision.start()]);
    let records = current(records);
    let matched: Vec<&Value> = records.iter().filter(|record| record["current"] == true
        && claim["evidence"].as_array().into_iter().flatten().filter_map(Value::as_str)
            .any(|reference| reference_matches(reference, record, &records))).collect();
    if matched.is_empty() { return false; }
    static FULL: OnceLock<Regex> = OnceLock::new();
    static LINES: OnceLock<Regex> = OnceLock::new();
    static READ: OnceLock<Regex> = OnceLock::new();
    static WRITE: OnceLock<Regex> = OnceLock::new();
    let requires_full = FULL.get_or_init(||Regex::new(r"(?i)全文|完整(?:读取|阅读|回读)|全部(?:内容|页面)|\b(?:entire|whole)\s+(?:file|document)|\bfull\s+(?:read|readback)|\ball\s+pages\b").expect("static complete read expression")).is_match(requirement);
    let line_range = LINES.get_or_init(|| Regex::new(r"(?i)(?:lines?\s*|第\s*)(\d+)\s*[-–至到]\s*(\d+)\s*(?:行)?").expect("static line range expression")).captures(requirement);
    let visual = task::requires_visual_observation(requirement);
    let explicit_read = requires_full || line_range.is_some()
        || READ.get_or_init(|| Regex::new(r"(?i)读取|阅读|回读|读回|\bread\b").expect("static explicit read expression")).is_match(requirement);
    let readback = task::requires_result_readback(requirement) || explicit_read;
    let writing = WRITE.get_or_init(|| Regex::new(r"(?i)写入|写\s+|创建|生成|保存|导出|\b(?:write|create|generate|save|export)\b").expect("static explicit write expression")).is_match(requirement);
    let expected = explicit_targets(requirement, &records);
    if writing {
        let writers: Vec<&Value> = matched.iter().copied().filter(|record|
            has_kind(record, "committed-output") || record["mutating"] == true && has_kind(record, "execution")).collect();
        if writers.is_empty() || expected.iter().any(|expected| !writers.iter().any(|writer| {
            let targets = writer["targets"].as_array();
            if targets.into_iter().flatten().any(|target| target["current"] == true && target_matches(target, expected)) { return true; }
            // Opaque commands need a versioned observation from the same execution segment.
            // 不透明命令须与同一执行段的真实目标版本观察组合，读取本身不能冒充写入。
            targets.is_none_or(Vec::is_empty) && writer["mutationSequence"].as_u64().is_some_and(|sequence|
                matched.iter().any(|observed| observed["observationCurrent"] != false
                    && observed["mutationSequence"].as_u64() == Some(sequence)
                    && (has_kind(observed, "observed-state") || has_kind(observed, "observed-content"))
                    && observed["targets"].as_array().into_iter().flatten().any(|target|
                        target["current"] == true && version_key(&target["version"]).is_some() && target_matches(target, expected))))
        })) { return false; }
    }
    let suitable: Vec<&Value> = matched.iter().copied().filter(|record| {
        if visual { has_kind(record, "visual") && record["visualObserved"] == true && record["observationCurrent"] != false }
        else if explicit_read { record["observationCurrent"] != false && has_kind(record, "observed-content") }
        else if readback { record["observationCurrent"] != false && ["observed-content", "observed-state", "validation"].iter().any(|kind| has_kind(record, kind)) }
        else if writing { has_kind(record, "committed-output") || record["mutating"] == true && has_kind(record, "execution")
            || record["observationCurrent"] != false && (has_kind(record, "observed-state") || has_kind(record, "observed-content")) }
        else { has_kind(record, "execution") }
    }).collect();
    if suitable.is_empty() { return false; }
    let targets: Vec<&Value> = suitable.iter().flat_map(|record| record["targets"].as_array().into_iter().flatten())
        .filter(|target| target["current"] == true).collect();
    for expected in &expected {
        if targets.iter().any(|target| target_matches(target, expected)) { continue; }
        // A combined save/render claim may join a committed target with an unlabelled image from the same turn.
        // 保存和渲染的复合要求可组合已提交目标与同轮无相反目标的图像。
        let combined_visual = visual && suitable.iter().filter(|image| image["targets"].as_array().is_none_or(Vec::is_empty))
            .any(|image| matched_commit_target(&records, claim, expected, image));
        if !combined_visual { return false; }
    }

    if requires_full {
        if expected.is_empty() {
            return !targets.is_empty() && complete_ranges(&targets);
        }
        return expected.iter().all(|expected| complete_ranges(&targets.iter().copied().filter(|target| target_matches(target, expected)).collect::<Vec<_>>()));
    }
    if let Some(range) = line_range {
        let (Some(start), Some(end)) = (range[1].parse::<u64>().ok(), range[2].parse::<u64>().ok()) else { return false; };
        return targets.iter().filter(|target| expected.is_empty() || expected.iter().any(|expected| target_matches(target, expected)))
            .any(|target| target["range"]["complete"] == true
            || target["range"]["startLine"].as_u64().is_some_and(|value| value <= start)
                && target["range"]["endLine"].as_u64().is_some_and(|value| value >= end));
    }
    true
}

fn matched_commit_target(records: &[Value], claim: &Value, expected: &str, image: &Value) -> bool {
    let Some(sequence) = image["mutationSequence"].as_u64() else { return false; };
    records.iter().any(|record| record["current"] == true && has_kind(record, "committed-output")
        && record["mutationSequence"].as_u64() == Some(sequence)
        && claim["evidence"].as_array().into_iter().flatten().filter_map(Value::as_str)
            .any(|reference| reference_matches(reference, record, records))
        && record["targets"].as_array().into_iter().flatten()
            .any(|target| target["current"] == true && target_matches(target, expected)))
}

pub(super) fn unresolved_check_summary(verification: &Value) -> Value {
    let unresolved = verification["unresolvedChecks"].as_array();
    let count = unresolved.map_or(0, Vec::len);
    json!({"count":count,"checks":unresolved.into_iter().flatten().take(8).map(|check|
        json!({"toolCallId":check["toolCallId"],"checkId":check["checkId"],
            "category":check["category"],"resolution":check["resolution"]})).collect::<Vec<_>>(),
        "remaining":count.saturating_sub(8)})
}

pub(super) fn directory(records: &[Value]) -> Value {
    json!(records.iter().filter(|record| record["current"] == true && record["tool"] != "tool.result.read")
        .map(|record| json!({"id":record["id"],"tool":record["tool"],"kinds":record["kinds"],
            "targets":record["targets"],"visualObserved":record["visualObserved"],
            "observationCurrent":record["observationCurrent"],"mutationSequence":record["mutationSequence"]})).collect::<Vec<_>>())
}


#[cfg(test)]
mod tests {
    use super::*;

    fn receipt(name: &str, id: &str, content: Value) -> Value {
        from_receipt(&json!({"name":name,"id":id,"arguments":{}}), &json!({"status":"completed","isError":false,"content":content}))
    }
    fn claim(ids: &[&str]) -> Value { json!({"evidence":ids}) }

    fn code_snapshot(path: &str, hash: &str) -> Value {
        let files = json!([{"pathId":format!("{:x}", Sha256::digest(path.as_bytes())),"sha256":hash,"state":"present"}]);
        json!({"signature":format!("{:x}", Sha256::digest(files.to_string().as_bytes())),"complete":true,"files":files})
    }

    fn scoped_check(progress: &mut RunProgress, id: &str, exit_code: i64, snapshot: &Value, scope: &ValidationScope) {
        progress.apply("progress.observeOutcome", &json!({
            "call":{"id":id,"name":"terminal.host.run","arguments":{"script":"cargo test"}},
            "result":{"status":"completed","isError":false},"canonicalResult":{"exitCode":exit_code,"cwd":"C:/work"},
            "beforeVersion":snapshot,"afterVersion":snapshot,
            "validationScope":{"cwd":"C:/work","acceptanceScope":scope.checkpoint()},
        })).unwrap();
    }

    #[test]
    fn withdrawn_target_checks_leave_current_delivery_without_erasing_failed_receipts() {
        let requirements = vec!["write a.rs".to_owned(), "write b.md".to_owned()];
        let records = vec![
            receipt("filesystem.write", "write-a", json!({"path":"C:/work/a.rs","sha256":"a".repeat(64),"bytes":4})),
            receipt("filesystem.write", "write-b", json!({"path":"C:/work/b.md","sha256":"b".repeat(64),"bytes":4})),
        ];
        let mut progress = RunProgress::default();
        progress.apply("progress.observeOutcome", &json!({"call":{"id":"write-a","name":"filesystem.write"},
            "result":{"status":"completed"},"canonicalResult":{"path":"C:/work/a.rs","sha256":"a".repeat(64),"bytes":4}})).unwrap();
        scoped_check(&mut progress, "failed-a", 1, &code_snapshot("C:/work/a.rs", &"a".repeat(64)),
            &validation_scope(&requirements, &records));
        let original = progress.apply("progress.state", &json!({})).unwrap();
        assert_eq!(progress.verification()["state"], "checks-failed");

        let active = vec![requirements[1].clone()];
        let scope = validation_scope(&active, &records);
        let review = CompletionEvidenceSet::from_records(progress.verification(), vec![json!("write-b")], records.clone())
            .with_current_requirements(&progress, &active);
        assert_eq!(review.verification["pendingValidation"], false);
        assert_eq!(review.verification["requiredChecks"][0]["scopeResolution"], "outside-current-requirements");
        let reply = json!({"state":"complete","source":"model-task-state","met":[
            {"requirementId":"requirement-1","item":"b.md","evidence":["write-b"]}]});
        let decision = task::TaskState::default().decide_typed(&review.decision_input(&active, task::TaskDecisionInput {
            is_review:true, evidence_required:true, requires_verification:true, reply:reply.clone(), ..Default::default()
        }));
        assert_eq!(serde_json::to_value(decision).unwrap()["action"], "final");
        let completion = progress.complete_for_scope(&json!({"modelTaskState":reply,
            "requiresVerification":true,"acceptanceReviewed":true,"brokerDecision":"final"}), &scope);
        assert_eq!(completion["completionState"], "completed");
        assert_eq!(completion["receipts"][0]["passed"], false);
        assert_eq!(progress.verification()["state"], "checks-failed");
        let after = progress.apply("progress.state", &json!({})).unwrap();
        assert_eq!(after["validationReceipts"], original["validationReceipts"]);
        assert_eq!(after["requiredChecks"], original["requiredChecks"]);
        assert!(after["validationReceipts"][0]["acceptanceScope"].is_object());
        assert!(completion["receipts"][0].get("acceptanceScope").is_none());
        assert!(completion["requiredChecks"][0].get("acceptanceScope").is_none());
        let mut restored = RunProgress::default();
        restored.apply("progress.restoreValidation", &json!({"state":after})).unwrap();
        assert_eq!(restored.verification_for_scope(&scope)["pendingValidation"], false);
    }

    #[test]
    fn shared_dependency_unknown_binding_and_changed_versions_keep_required_checks_active() {
        let requirements = vec!["write a.rs".to_owned(), "write b.md".to_owned()];
        let records = vec![
            receipt("filesystem.write", "write-a", json!({"path":"C:/work/a.rs","sha256":"a".repeat(64),"bytes":4})),
            receipt("filesystem.write", "write-b", json!({"path":"C:/work/b.md","sha256":"b".repeat(64),"bytes":4})),
        ];
        let mut progress = RunProgress::default();
        scoped_check(&mut progress, "failed-a", 1, &code_snapshot("C:/work/a.rs", &"a".repeat(64)),
            &validation_scope(&requirements, &records));
        for active in [vec!["write b.md and verify a.rs".to_owned()], vec!["finish the remaining feature".to_owned()]] {
            let view = progress.verification_for_scope(&validation_scope(&active, &records));
            assert_eq!(view["pendingValidation"], true);
            assert_eq!(view["requiredChecks"][0]["active"], true);
        }
        let mut changed = records.clone();
        changed[0]["targets"][0]["version"] = json!("c".repeat(64));
        assert_eq!(progress.verification_for_scope(&validation_scope(&["write a.rs".to_owned()], &changed))["pendingValidation"], true);

        let mut legacy = progress.apply("progress.state", &json!({})).unwrap();
        for field in ["validationReceipts", "requiredChecks"] {
            for check in legacy[field].as_array_mut().unwrap() { check.as_object_mut().unwrap().remove("acceptanceScope"); }
        }
        let mut restored = RunProgress::default();
        restored.apply("progress.restoreValidation", &json!({"state":legacy})).unwrap();
        let active = vec![requirements[1].clone()];
        assert_eq!(restored.verification_for_scope(&validation_scope(&active, &records))["pendingValidation"], true);

        let active = vec![requirements[0].clone()];
        let scope = validation_scope(&active, &records);
        let reply = json!({"state":"complete","source":"model-task-state","met":[
            {"requirementId":"requirement-1","item":"a.rs","evidence":["write-a"]}]});
        let review = CompletionEvidenceSet::from_records(progress.verification(), vec![json!("write-a")], records.clone())
            .with_current_requirements(&progress, &active);
        assert!(matches!(task::TaskState::default().decide_typed(&review.decision_input(&active, task::TaskDecisionInput {
            is_review:true, evidence_required:true, reply:reply.clone(), ..Default::default()
        })), task::TaskDecision::Resume { .. }));
        scoped_check(&mut progress, "passed-current-a", 0, &code_snapshot("C:/work/a.rs", &"a".repeat(64)), &scope);
        assert_eq!(progress.verification_for_scope(&scope)["pendingValidation"], false);
        let review = CompletionEvidenceSet::from_records(progress.verification(), vec![json!("write-a")], records.clone())
            .with_current_requirements(&progress, &active);
        assert!(matches!(task::TaskState::default().decide_typed(&review.decision_input(&active, task::TaskDecisionInput {
            is_review:true, evidence_required:true, reply:reply.clone(), ..Default::default()
        })), task::TaskDecision::Final { .. }));
        assert_eq!(progress.complete_for_scope(&json!({"modelTaskState":reply,"acceptanceReviewed":true,
            "requiresVerification":true,"brokerDecision":"final"}), &scope)["completionState"], "completed");
        assert_eq!(progress.verification()["receipts"][0]["passed"], false);
    }

    #[test]
    fn optional_check_failure_does_not_block_real_alternative_readback() {
        let requirements = vec!["read b.txt".to_owned()];
        let read = receipt("document.read", "alternative-read", json!({"path":"C:/work/b.txt","sha256":"b".repeat(64),
            "content":"body","offset":0,"nextOffset":4,"totalCharacters":4}));
        let mut progress = RunProgress::default();
        progress.apply("progress.observeOutcome", &json!({"call":{"id":"failed-read","name":"filesystem.read"},
            "result":{"status":"error","isError":true,"content":{"code":"FORMAT_UNSUPPORTED"}}})).unwrap();
        progress.apply("progress.observeOutcome", &json!({"call":{"id":"alternative-read","name":"document.read"},
            "result":{"status":"completed","isError":false},"canonicalResult":{"path":"C:/work/b.txt","sha256":"b".repeat(64),
                "content":"body","offset":0,"nextOffset":4,"totalCharacters":4}})).unwrap();
        let records = vec![receipt("filesystem.read", "unrelated-source", json!({
            "path":"C:/work/unrelated.rs","sha256":"c".repeat(64),"content":"source"})), read];
        let scope = validation_scope(&requirements, &records);
        scoped_check(&mut progress, "optional-unrelated-check", 1, &code_snapshot("C:/work/unrelated.rs", &"c".repeat(64)), &scope);
        let review = CompletionEvidenceSet::from_records(progress.verification(), vec![json!("alternative-read")], records)
            .with_current_requirements(&progress, &requirements);
        let reply = json!({"state":"complete","source":"model-task-state","met":[
            {"requirementId":"requirement-1","item":"read b.txt","evidence":["alternative-read"]}]});
        let decision = task::TaskState::default().decide_typed(&review.decision_input(&requirements, task::TaskDecisionInput {
            is_review:true, evidence_required:true, requires_verification:true, reply:reply.clone(), ..Default::default()
        }));
        assert_eq!(serde_json::to_value(decision).unwrap()["action"], "final");
        assert_eq!(progress.complete_for_scope(&json!({"modelTaskState":reply,"acceptanceReviewed":true,
            "requiresVerification":true,"brokerDecision":"final"}), &scope)["completionState"], "completed");
        assert_eq!(progress.verification()["receipts"][0]["passed"], false);
    }

    #[test]
    fn windows_target_aliases_preserve_shared_checks_and_missing_native_maps_stay_active() {
        let requirements = vec!["write a.rs".to_owned(), "write b.md".to_owned()];
        let records = vec![
            receipt("filesystem.write", "write-a", json!({"path":"C:/Work/A.rs","sha256":"a".repeat(64),"bytes":4})),
            receipt("filesystem.write", "write-b", json!({"path":"C:/work/b.md","sha256":"b".repeat(64),"bytes":4})),
        ];
        let mut progress = RunProgress::default();
        scoped_check(&mut progress, "check-original-alias", 1,
            &code_snapshot("C:/Work/A.rs", &"a".repeat(64)), &validation_scope(&requirements, &records));
        let aliases = vec![receipt("filesystem.read", "read-a", json!({
            "path":"c:\\work\\a.rs","sha256":"a".repeat(64),"content":"source"}))];
        let scope = validation_scope(&["read a.rs".to_owned()], &aliases);
        assert_ne!(records[0]["targets"][0]["nativePathId"], aliases[0]["targets"][0]["nativePathId"]);
        assert_eq!(records[0]["targets"][0]["identity"], aliases[0]["targets"][0]["identity"]);
        assert_eq!(progress.verification_for_scope(&scope)["pendingValidation"], true);

        let mut legacy = progress.apply("progress.state", &json!({})).unwrap();
        for field in ["validationReceipts", "requiredChecks"] {
            for check in legacy[field].as_array_mut().unwrap() {
                check["acceptanceScope"]["scope"].as_object_mut().unwrap().remove("knownTargets");
                for requirement in check["acceptanceScope"]["scope"]["requirements"].as_array_mut().unwrap() {
                    for target in requirement["targets"].as_array_mut().unwrap() {
                        target.as_object_mut().unwrap().remove("identity");
                    }
                }
            }
        }
        let mut restored = RunProgress::default();
        restored.apply("progress.restoreValidation", &json!({"state":legacy})).unwrap();
        assert_eq!(restored.verification_for_scope(&validation_scope(&[requirements[1].clone()], &records))["pendingValidation"], true);
    }

    #[test]
    fn evidence_targets_cannot_cross_and_labels_cannot_rebind_them() {
        let a = receipt("filesystem.write", "save-a", json!({"path":"C:/work/a.docx","sha256":"a".repeat(64),"bytes":10}));
        assert!(supports_claim(&claim(&["save-a"]), "save a.docx", &[a.clone()]));
        assert!(supports_claim(&claim(&["save-a"]), "请创建a.docx", &[a.clone()]));
        assert!(!supports_claim(&json!({"item":"save b.docx","evidence":["save-a"]}), "save b.docx", &[a]));
        let write = receipt("filesystem.write", "save-a", json!({"path":"C:/work/a.txt","sha256":"a".repeat(64),"bytes":10}));
        let read = receipt("filesystem.read", "read-b", json!({"path":"C:/work/b.txt","sha256":"b".repeat(64),"content":"body"}));
        assert!(supports_claim(&claim(&["save-a","read-b"]), "创建a.txt并读取b.txt", &[write,read]));
    }

    #[test]
    fn annotated_call_ids_match_only_the_complete_trusted_identity() {
        let write = receipt("filesystem.write", "call_00_save_a", json!({"path":"C:/work/a.txt","sha256":"a".repeat(64),"bytes":10}));
        for reference in ["call_00_save_a: 文件已保存", "call_00_save_a： Saved a.txt", "call_00_save_a : 保存回执"] {
            assert!(supports_claim(&claim(&[reference]), "save a.txt", &[write.clone()]), "{reference}");
        }
        for reference in ["other: call_00_save_a", "call_00_save_a_extra: saved", "call_00_save_a saved",
            "call_00_save_a:", "call_00_save_a: /work/a.txt", "unknown: call_00_save_a: saved"] {
            assert!(!supports_claim(&claim(&[reference]), "save a.txt", &[write.clone()]), "{reference}");
        }
        assert!(!annotated_call_reference("C:/work/a.txt", "C"));
        assert!(!annotated_call_reference(r"C:\work\a.txt", "C"));
        assert!(!annotated_call_reference("https://example.com", "https"));
    }

    #[test]
    fn annotated_ids_do_not_change_receipt_actions_targets_versions_or_ranges() {
        let write = receipt("filesystem.write", "call_save_a", json!({"path":"C:/work/a.txt","sha256":"a".repeat(64),"bytes":10}));
        let labelled = json!({"item":"已保存 b.txt", "evidence":["call_save_a: 已保存 b.txt"]});
        assert!(!supports_claim(&labelled, "save b.txt", &[write.clone()]));
        let read = receipt("filesystem.read", "call_read_a", json!({"path":"C:/work/a.txt","sha256":"a".repeat(64),
            "offset":0,"nextOffset":5,"totalCharacters":10,"hasMore":true}));
        assert!(supports_claim(&claim(&["call_read_a: 已写入并渲染"]), "read a.txt", &[read.clone()]));
        for requirement in ["save a.txt", "render the scene", "完整读取 a.txt"] {
            assert!(!supports_claim(&claim(&["call_read_a: 已完整保存并渲染"]), requirement, &[read.clone()]), "{requirement}");
        }
        let latest = receipt("filesystem.write", "call_save_new", json!({"path":"C:/work/a.txt","sha256":"b".repeat(64),"bytes":10}));
        assert!(!supports_claim(&claim(&["call_save_a: 最新保存版本"]), "save a.txt", &[write,latest]));
    }

    #[test]
    fn annotated_ids_cannot_restore_failed_unknown_or_inactive_evidence() {
        for status in ["failed", "unknown", "running"] {
            let proof = from_receipt(&json!({"id":"call_bad","name":"filesystem.read"}),
                &json!({"status":status,"content":{"path":"C:/work/a.txt"}}));
            assert!(proof.is_null());
            assert!(!supports_claim(&claim(&["call_bad: 成功读取"]), "read a.txt", &[proof]));
        }
        let error = from_receipt(&json!({"id":"call_error","name":"filesystem.read"}),
            &json!({"status":"completed","isError":true,"content":{"path":"C:/work/a.txt"}}));
        assert!(!supports_claim(&claim(&["call_error: 成功读取"]), "read a.txt", &[error]));
        let mut inactive = receipt("filesystem.read", "call_inactive", json!({"path":"C:/work/a.txt","sha256":"a".repeat(64)}));
        inactive["current"] = json!(false);
        assert!(!supports_claim(&claim(&["call_inactive: 当前有效"]), "read a.txt", &[inactive]));
    }

    #[test]
    fn annotated_reference_compatibility_preserves_exact_paths_and_basename_rules() {
        let first = receipt("filesystem.write", "call_save_a", json!({"path":"C:/work/a.txt","sha256":"a".repeat(64),"bytes":10}));
        assert!(supports_claim(&claim(&["C:/work/a.txt"]), "save a.txt", &[first.clone()]));
        assert!(supports_claim(&claim(&[r"C:\work\a.txt"]), "save a.txt", &[first.clone()]));
        assert!(!supports_claim(&claim(&["C:/work/a.txt: saved"]), "save a.txt", &[first.clone()]));
        assert!(!supports_claim(&claim(&["a.txt: saved"]), "save a.txt", &[first.clone()]));
        let other = receipt("filesystem.write", "call_other_a", json!({"path":"C:/other/a.txt","sha256":"b".repeat(64),"bytes":10}));
        assert!(!supports_claim(&claim(&["a.txt"]), "save a.txt", &[first,other]));
    }

    #[test]
    fn conditional_action_decisions_do_not_require_the_revoked_effect() {
        let read = receipt("filesystem.read", "read-a", json!({"path":"C:/work/a.txt",
            "sha256":"a".repeat(64),"offset":0,"nextOffset":10,"totalCharacters":10,"hasMore":false}));
        for requirement in ["先读取 a.txt，根据当前已确认全局记忆决定是否写入两个文件。",
            "Read a.txt, then decide whether to write b.txt", "Decide whether to create b.txt"] {
            assert!(supports_claim(&claim(&["read-a"]), requirement, &[read.clone()]), "{requirement}");
        }
        assert!(!supports_claim(&claim(&["read-a"]), "读取 a.txt 然后写入 b.txt", &[read.clone()]));
        assert!(!supports_claim(&claim(&["read-a"]), "判断是否已经完整保存 b.txt", &[read]));
    }

    #[test]
    fn writes_and_plain_terminal_exits_are_not_quality_or_visual_proof() {
        let write = receipt("filesystem.write", "save", json!({"path":"C:/work/a.txt","sha256":"a".repeat(64),"bytes":10}));
        let read = receipt("filesystem.read", "read", json!({"path":"C:/work/a.txt","sha256":"a".repeat(64),"content":"body"}));
        let stat = receipt("filesystem.stat", "stat", json!({"path":"C:/work/a.txt","sha256":"a".repeat(64)}));
        assert!(!supports_claim(&claim(&["save"]), "read a.txt", &[write.clone()]));
        assert!(!supports_claim(&claim(&["stat"]), "read a.txt", &[stat]));
        assert!(!supports_claim(&claim(&["read"]), "save a.txt", &[read]));
        let terminal = from_receipt(&json!({"name":"terminal.host.run","id":"command","arguments":{"command":"echo done"}}),
            &json!({"status":"completed","content":{"exitCode":0,"stdout":"done"}}));
        assert!(!supports_claim(&claim(&["save"]), "verify file a.txt", &[write]));
        assert!(!supports_claim(&claim(&["command"]), "verify project quality", &[terminal]));
        let mut image = receipt("browser.screenshot", "image", json!({"content":[{"type":"image","mimeType":"image/png","data":"aW1hZ2U="}]}));
        assert!(!supports_claim(&claim(&["image"]), "render the scene", &[image.clone()]));
        image["visualObserved"] = json!(true);
        assert!(supports_claim(&claim(&["image"]), "render the scene", &[image]));
    }

    #[test]
    fn image_read_requires_actual_visual_delivery_and_retention_is_not_observation() {
        let image = receipt("image.read", "local-image", json!({
            "structuredContent":{"path":"C:/work/scene.png","sha256":"a".repeat(64)},
            "content":[{"type":"image","mimeType":"image/png","data":"aW1hZ2U="}]}));
        assert!(has_kind(&image, "observed-content"));
        assert!(has_kind(&image, "visual"));
        assert_eq!(image["visualObserved"], false);
        assert!(!supports_claim(&claim(&["local-image"]), "inspect visual quality", &[image]));
        let retained = receipt("image.retain", "retain-image", json!({"resultId":"saved","retained":1}));
        assert!(!has_kind(&retained, "observed-content"));
        assert!(!has_kind(&retained, "visual"));
        assert!(!supports_claim(&claim(&["retain-image"]), "inspect visual quality", &[retained]));
    }

    #[test]
    fn changed_hash_invalidates_old_target_without_erasing_it() {
        let old = receipt("filesystem.read", "old", json!({"path":"C:/work/a.txt","sha256":"a".repeat(64),"content":"old"}));
        let mut checkpoint = receipt("task.document", "record", json!({"path":"C:/work/a.txt",
            "sourceVersion":{"sha256":"a".repeat(64)}}));
        let unchanged = current(&[old.clone(), checkpoint.clone()]);
        assert_eq!(unchanged[0]["current"], true);
        assert!(supports_claim(&claim(&["old"]), "读取 a.txt", &unchanged));
        assert!(!supports_claim(&claim(&["record"]), "读取 a.txt", &unchanged));
        checkpoint["targets"][0]["version"] = json!({"sha256":"b".repeat(64)});
        assert!(!supports_claim(&claim(&["old"]), "读取 a.txt", &[old.clone(), checkpoint]));
        let new = receipt("filesystem.write", "new", json!({"path":"C:/work/a.txt","sha256":"b".repeat(64),"bytes":10}));
        let records = current(&[old,new]);
        assert_eq!(records.len(),2);
        assert_eq!(records[0]["current"],false);
        assert!(!supports_claim(&claim(&["old"]),"verify file a.txt",&records));
    }

    #[test]
    fn full_reads_merge_matching_current_ranges_but_not_partial_or_mixed_versions() {
        let first=receipt("filesystem.read","first",json!({"path":"C:/work/a.txt","sha256":"a".repeat(64),"offset":0,"nextOffset":5,"totalCharacters":10,"hasMore":true,"documentComplete":true,"extractionComplete":true}));
        let second=receipt("filesystem.read","second",json!({"path":"C:/work/a.txt","sha256":"a".repeat(64),"offset":5,"nextOffset":10,"totalCharacters":10,"hasMore":false}));
        assert!(!supports_claim(&claim(&["first"]),"完整读取 a.txt",&[first.clone()]));
        assert!(supports_claim(&claim(&["first","second"]),"完整读取 a.txt",&[first.clone(),second.clone()]));
        let mut changed=second;changed["targets"][0]["version"]=json!("b".repeat(64));
        assert!(!supports_claim(&claim(&["first","second"]),"完整读取 a.txt",&[first,changed]));
    }

    #[test]
    fn a_composite_receipt_can_cover_multiple_requirements() {
        let mut proof=receipt("filesystem.read","read",json!({"path":"C:/work/a.txt","sha256":"a".repeat(64),"offset":0,"nextOffset":10,"totalCharacters":10,"hasMore":false}));
        proof["targets"].as_array_mut().unwrap().push(json!({"kind":"file","identity":"c:/work/b.txt","version":"b".repeat(64),"range":{"complete":true}}));
        for goal in ["verify file a.txt","verify file b.txt"] {assert!(supports_claim(&claim(&["read"]),goal,&[proof.clone()]));}
        assert!(supports_claim(&claim(&["read"]),"继续完善内容",&[proof]));
        let read = receipt("filesystem.read","source",json!({"path":"C:/work/a.txt","sha256":"a".repeat(64),"content":"source"}));
        let write = receipt("filesystem.write","save",json!({"path":"C:/work/b.txt","sha256":"b".repeat(64),"bytes":6}));
        for goal in ["读取a.txt，写入b.txt", "读取 a.txt，随后写入 b.txt", "read a.txt, write b.txt", "Read a.txt; then save b.txt"] {
            assert!(supports_claim(&claim(&["source","save"]),goal,&[read.clone(),write.clone()]),"{goal}");
            assert!(!supports_claim(&claim(&["source"]),goal,&[read.clone(),write.clone()]),"{goal}");
            assert!(!supports_claim(&claim(&["save"]),goal,&[read.clone(),write.clone()]),"{goal}");
        }
        let output_read = receipt("filesystem.read","output-read",json!({"path":"C:/work/b.txt","sha256":"b".repeat(64),"content":"output"}));
        assert!(supports_claim(&claim(&["source","save","output-read"]),"读取 a.txt，写 b.txt 并读回，最后回复交付结果",&[read.clone(),write.clone(),output_read]));
        assert!(!supports_claim(&claim(&["source","save"]),"读取 a.txt，写 b.txt 并读回 b.txt，最后回复交付结果",&[read,write]));
        for goal in [r#"read "a,write b.txt""#, "读取「a，写入b.txt」", "读取 `a;save b.txt`",
            "read https://example.com/a,write.json", "读取https://example.com/a;write.json"] {
            assert_eq!(acceptance_clauses(goal),vec![goal],"{goal}");
        }
        assert_eq!(acceptance_clauses(r#"read "a,write b.txt", write c.txt"#),vec![r#"read "a,write b.txt""#," write c.txt"]);
        assert_eq!(acceptance_clauses("read https://example.com/a, write c.txt"),vec!["read https://example.com/a"," write c.txt"]);
    }

    #[test]
    fn combined_visual_proof_needs_the_same_turn_and_rejects_a_different_known_target() {
        let mut saved=receipt("filesystem.write","save",json!({"path":"C:/work/a.blend","sha256":"a".repeat(64),"bytes":10}));
        saved["mutationSequence"]=json!(3);
        let mut image=receipt("browser.screenshot","image",json!({"content":[{"type":"image","mimeType":"image/png","data":"aW1hZ2U="}]}));
        image["visualObserved"]=json!(true);image["mutationSequence"]=json!(3);
        assert!(supports_claim(&claim(&["save","image"]),"save and render a.blend",&[saved.clone(),image.clone()]));
        image["targets"]=json!([{"kind":"file","identity":"c:/work/b.blend","version":"b","range":{}}]);
        assert!(!supports_claim(&claim(&["save","image"]),"save and render a.blend",&[saved.clone(),image.clone()]));
        image["targets"]=json!([]);image["mutationSequence"]=json!(2);
        assert!(!supports_claim(&claim(&["save","image"]),"save and render a.blend",&[saved,image]));
    }

    #[test]
    fn validation_needs_broker_version_proof_and_line_reads_need_the_requested_range() {
        let mut check=from_receipt(&json!({"name":"terminal.host.run","id":"check","arguments":{"command":"cargo test"}}),
            &json!({"status":"completed","content":{"exitCode":0}}));
        assert!(!supports_claim(&claim(&["check"]),"test project quality",&[check.clone()]));
        check["validation"]=json!({"passed":true,"coversObservedRevision":true,"codeVersionCoverage":{"sameObservedVersion":true}});
        assert!(supports_claim(&claim(&["check"]),"test project quality",&[check.clone()]));
        assert!(supports_claim(&claim(&["check"]),"完整检查代码并运行测试",&[check]));
        let read=receipt("filesystem.read","read",json!({"path":"C:/work/a.txt","sha256":"a".repeat(64),"lineRange":{"startLine":3,"endLine":8}}));
        assert!(supports_claim(&claim(&["read"]),"read file a.txt lines 3-8",&[read.clone()]));
        assert!(!supports_claim(&claim(&["read"]),"read file a.txt lines 1-8",&[read]));
    }

    #[test]
    fn unsuccessful_receipts_and_empty_structured_evidence_cannot_prove_completion() {
        for status in ["unknown","error","running"] {
            assert!(from_receipt(&json!({"id":"bad","name":"filesystem.read"}),&json!({"status":status,"content":{"path":"C:/work/a.txt"}})).is_null());
        }
        assert!(!supports_claim(&claim(&["invented"]),"save a.txt",&[]));
    }

    #[test]
    fn actual_sources_and_launch_paths_are_bound_without_arguments_or_false_full_reads() {
        let read=receipt("knowledge.read","read",json!({"sourceRef":"ev1:opaque","bindingRevision":3,"locator":{"relativePath":"a.md"},"offset":0,"nextOffset":5,"totalCharacters":10,"documentComplete":true}));
        assert!(supports_claim(&claim(&["read"]),"读取a.md",&[read.clone()]));
        assert!(!supports_claim(&claim(&["read"]),"全文读取a.md",&[read]));
        let launch=receipt("desktop.app.launch","launch",json!({"appPath":"D:/Blender/blender.exe","pid":123}));
        assert!(supports_claim(&claim(&["launch"]),"打开 blender.exe",&[launch]));
        let search=receipt("web.search","search",json!({"items":[{"url":"https://example.com/a"}]}));
        assert!(supports_claim(&claim(&["search"]),"检查 https://example.com/a",&[search]));
    }

    #[test]
    fn later_mutations_preserve_execution_history_but_invalidate_unlabelled_observations() {
        let mut command=receipt("terminal.host.run","run",json!({"exitCode":0}));
        command["mutationSequence"]=json!(1);command["mutating"]=json!(true);
        let mut next=receipt("filesystem.write","save",json!({"path":"C:/work/a.txt","sha256":"a".repeat(64),"bytes":10}));
        next["mutationSequence"]=json!(2);next["mutating"]=json!(true);
        assert!(supports_claim(&claim(&["run"]),"执行命令",&[command.clone(),next.clone()]));
        command["kinds"]=json!(["execution","observed-state"]);
        assert!(!supports_claim(&claim(&["run"]),"检查软件状态",&[command,next]));
    }

    #[test]
    fn scoped_observations_survive_unrelated_changes_but_not_target_revisions() {
        let mut read = receipt("filesystem.read", "read-a", json!({"path":"C:/work/a.txt","sha256":"a".repeat(64),"content":"body"}));
        read["mutationSequence"] = json!(0);
        let mut image = receipt("mcp.fixture.look", "scene-image", json!({"sourceId":"scene-main","sourceVersion":1,
            "content":[{"type":"image","mimeType":"image/png","data":"aW1hZ2U="}]}));
        image["mutationSequence"] = json!(0); image["visualObserved"] = json!(true);
        let mut log = receipt("filesystem.write", "save-log", json!({"path":"C:/work/log.txt","sha256":"b".repeat(64),"bytes":5}));
        log["mutationSequence"] = json!(1); log["mutating"] = json!(true);
        let mut records = vec![read, image, log];
        assert!(supports_claim(&claim(&["read-a"]), "read a.txt", &records));
        assert!(has_readback(&current(&records), true));
        let mut changed = receipt("filesystem.write", "change-a", json!({"path":"C:/work/a.txt","sha256":"c".repeat(64),"bytes":5}));
        changed["mutationSequence"] = json!(2); changed["mutating"] = json!(true);
        records.push(changed);
        assert!(!supports_claim(&claim(&["read-a"]), "read a.txt", &records));
        assert!(has_readback(&current(&records), true));
        let mut scene = receipt("mcp.fixture.execute_blender_code", "change-scene", json!({"sourceId":"scene-main","sourceVersion":2}));
        scene["mutationSequence"] = json!(3); scene["mutating"] = json!(true);
        records.push(scene);
        assert!(!has_readback(&current(&records), true));
        assert_eq!(current(&current(&records)), current(&records));
    }

    #[test]
    fn opaque_changes_and_unversioned_target_writes_invalidate_old_observations() {
        let mut read = receipt("filesystem.read", "read-a", json!({"path":"C:/work/a.txt","sha256":"a".repeat(64),"content":"body"}));
        read["mutationSequence"] = json!(0);
        let mut opaque = receipt("terminal.host.run", "unknown-scope", json!({"exitCode":0}));
        opaque["mutationSequence"] = json!(1); opaque["mutating"] = json!(true);
        let records = current(&[read.clone(), opaque]);
        assert!(!has_readback(&records, false));
        assert!(!supports_claim(&claim(&["read-a"]), "read a.txt", &records));
        assert!(supports_claim(&claim(&["read-a"]), "执行操作", &records));
        let mut changed = receipt("filesystem.write", "unversioned-write", json!({"path":"C:/work/a.txt","bytes":5}));
        changed["mutationSequence"] = json!(1); changed["mutating"] = json!(true);
        assert!(!has_readback(&current(&[read.clone(), changed]), false));
        let mut missing = receipt("filesystem.read", "read-after-gap", json!({"path":"C:/work/b.txt","sha256":"b".repeat(64)}));
        missing["mutationSequence"] = json!(2); missing["mutating"] = json!(false);
        assert!(!supports_claim(&claim(&["read-a"]), "read a.txt", &[read, missing]));
    }

    #[test]
    fn visual_proof_requires_delivery_and_container_paths_do_not_scope_the_scene() {
        let mut log = receipt("filesystem.write", "save-log", json!({"path":"C:/work/log.txt","sha256":"b".repeat(64),"bytes":5}));
        log["mutationSequence"] = json!(1); log["mutating"] = json!(true);
        for name in ["browser.screenshot", "filesystem.read"] {
            for source in [json!({}), json!({"path":"C:/work/render.png","sha256":"a".repeat(64)})] {
                let mut content = source;
                content["content"] = json!([{"type":"image","mimeType":"image/png","data":"aW1hZ2U="}]);
                let mut image = receipt(name, "image", content);
                image["mutationSequence"] = json!(0);
                assert!(!has_readback(&current(&[image.clone()]), true));
                image["visualObserved"] = json!(true);
                let restored = current(&[image.clone()]);
                assert!(has_readback(&restored, true));
                assert!(!has_readback(&current(&[image, log.clone()]), true));
            }
        }
    }

    #[test]
    fn opaque_writes_need_a_matching_versioned_observation_from_the_same_segment() {
        let mut command = receipt("skill.run", "generate", json!({"exitCode":0}));
        command["mutationSequence"] = json!(4); command["mutating"] = json!(true);
        let mut read = receipt("filesystem.read", "read", json!({"path":"C:/work/a.docx","sha256":"a".repeat(64),"content":"body"}));
        read["mutationSequence"] = json!(4);
        assert!(supports_claim(&claim(&["generate","read"]), "save a.docx", &[command.clone(),read.clone()]));
        assert!(!supports_claim(&claim(&["read"]), "save a.docx", &[command.clone(),read.clone()]));
        assert!(!supports_claim(&claim(&["generate"]), "save a.docx", &[command.clone(),read.clone()]));
        read["mutationSequence"] = json!(3);
        assert!(!supports_claim(&claim(&["generate","read"]), "save a.docx", &[command,read]));
    }

    #[test]
    fn superseded_or_out_of_scope_checks_cannot_reuse_an_old_success() {
        let mut proof = receipt("terminal.host.run", "old-success", json!({"exitCode":0}));
        proof["validation"] = json!({"toolCallId":"old-success","passed":true,"coversObservedRevision":true,
            "codeVersionCoverage":{"sameObservedVersion":true}});
        assert!(supports_claim(&claim(&["old-success"]), "test project quality", &[proof.clone()]));
        let verification = json!({"receipts":[{"toolCallId":"old-success","passed":true,"coversObservedRevision":true,
            "codeVersionCoverage":{"sameObservedVersion":true},"current":false,"active":true}]});
        bind_validation(&mut proof, &verification);
        assert!(!supports_claim(&claim(&["old-success"]), "test project quality", &[proof.clone()]));
        proof["validation"]["current"] = json!(true);
        proof["validation"]["active"] = json!(false);
        assert!(!supports_claim(&claim(&["old-success"]), "test project quality", &[proof]));
    }
}

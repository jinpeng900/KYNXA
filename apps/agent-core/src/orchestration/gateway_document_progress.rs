//! Per-document public work records use the existing task artifact store, not a second ledger.
//! 逐文档公开工作结果复用既有任务产物存储；不再建立第二份状态账本或复制原文。

use super::gateway_task_journal::TaskJournal;
use crate::gateway_contract::GatewayError;
use crate::models::tool_context::source_metadata;
use crate::storage::managed_files;
use serde_json::{Value, json};
use std::collections::BTreeMap;

type Result<T> = std::result::Result<T, GatewayError>;

/// In-memory indexes only accelerate the registered, scope-bound artifact records.
/// 内存目录只加速已登记且绑定任务作用域的产物记录；断点仍保存正式产物引用。
#[derive(Default)]
pub struct DocumentWorkRecords {
    records: BTreeMap<String, Value>,
    references: BTreeMap<String, Value>,
}

impl DocumentWorkRecords {
    /// Call only after a successful read and the original result archive have been accepted.
    /// 仅在实际读取成功且原回执已正式归档后调用；元数据不会被当成完成的语义分析。
    pub async fn observe_read(
        &mut self,
        journal: &TaskJournal,
        tool: &str,
        receipt: &Value,
        result_ref: &Value,
    ) -> Result<Option<Value>> {
        let source = source_metadata(receipt);
        if !matches!(
            tool,
            "filesystem.read" | "document.read" | "knowledge.read" | "web.fetch" | "tool.result.read"
        ) || !result_ref["id"].as_str().is_some_and(|id| !id.is_empty())
            || source["sourceTool"] == "context.working-summary"
            || !has_document_locator(&source)
        {
            return Ok(None);
        }
        let mut record = self.observed_record(tool, &source, result_ref)?;
        record["active"] = json!(true);
        record.as_object_mut().unwrap().remove("invalidatedReason");
        let key = text(&record["sourceIdentity"]).to_owned();
        let information_added = self.records.get(&key).is_none_or(|previous| {
            previous["active"] != true || adds_read_coverage(previous, &record)
        });
        self.invalidate_superseded(journal, &record).await?;
        self.persist_record(journal, record).await?;
        let mut entry = self.entry(&key);
        entry["informationAdded"] = json!(information_added);
        Ok(Some(entry))
    }

    /// Main-model analysis is stored without a second model call or a claim of semantic proof.
    /// 主模型分析直接保存，不额外调用模型；结论绑定实际读取，仍不声明语义正确或任务完成。
    pub async fn record_analysis(
        &mut self,
        journal: &TaskJournal,
        result_id: &str,
        conclusions: &[String],
        unresolved: &[String],
        output_refs: &[Value],
    ) -> Result<Value> {
        if conclusions
            .iter()
            .chain(unresolved)
            .any(|value| value.trim().is_empty())
            || output_refs.iter().any(|value| !value.is_object())
        {
            return Err(invalid(
                "Analysis entries or output references are invalid. / 分析条目或产物引用无效。",
            ));
        }
        let key = self.records.iter().find_map(|(key, record)| {
            (record["active"] == true && array(&record["observations"]).iter().any(|source| {
                source["resultRef"]["id"].as_str() == Some(result_id)
            })).then(|| key.clone())
        }).ok_or_else(|| invalid("Analysis must refer to an active, actually read document. / 分析须绑定当前有效且实际读取的文档。"))?;
        let mut record = self.records[&key].clone();
        let same_analysis = record["analysis"]["state"] == "recorded"
            && record["analysis"]["conclusions"] == json!(conclusions)
            && record["analysis"]["unresolved"] == json!(unresolved)
            && record["analysis"]["outputReferences"] == json!(output_refs);
        if same_analysis {
            let mut entry = self.entry(&key);
            entry["informationAdded"] = json!(false);
            return Ok(entry);
        }
        record["analysis"] = json!({
            "state":"recorded", "resultId":result_id, "conclusions":conclusions,
            "unresolved":unresolved, "unresolvedStatus":"declared-by-model",
            "outputReferences":output_refs, "outputsVerified":false,
            "semanticCorrectnessCertified":false,
            "notice":"Recorded analysis is reusable work, not proof that every requirement is satisfied. / 已保存分析可用于续接，不代表所有要求已核验完成。"
        });
        self.persist_record(journal, record).await?;
        let mut entry = self.entry(&key);
        entry["informationAdded"] = json!(true);
        Ok(entry)
    }

    /// Accepted compaction facts supplement document records without replacing explicit analysis.
    /// 已接受的压缩事实补入逐文档记录；不能覆盖主模型显式保存的完整分析结果。
    pub async fn update_from_summary(
        &mut self,
        journal: &TaskJournal,
        summary: &Value,
    ) -> Result<Value> {
        let mut updates = BTreeMap::new();
        for cursor in array(&summary["sources"]) {
            let source = source_metadata(&cursor["source"]);
            if !has_document_locator(&source) {
                continue;
            }
            let result_ref = &cursor["resultRef"];
            let result_id = text(&result_ref["id"]);
            if result_id.is_empty() {
                continue;
            }
            let mut record = self.observed_record(text(&cursor["tool"]), &source, result_ref)?;
            let key = text(&record["sourceIdentity"]).to_owned();
            // An archived summary cannot roll the current document version back.
            // 旧归档摘要不能让当前文档版本回退；只有真实新读取可以切换活动版本。
            if record["active"] != true
                || self.records.iter().any(|(existing_key, existing)| {
                    existing_key != &key
                        && existing["active"] == true
                        && existing["sourceTarget"] == record["sourceTarget"]
                })
            {
                continue;
            }
            if let Some(previous) = updates.remove(&key) {
                record = merge_observations(previous, &record);
            }
            let facts: Vec<Value> = array(&summary["facts"])
                .iter()
                .filter(|fact| {
                    fact["resultId"] == result_ref["id"] && !text(&fact["text"]).trim().is_empty()
                })
                .cloned()
                .collect();
            replace_result_items(&mut record["summaryFacts"], result_id, facts);
            let processed = cursor["processedCharacters"].as_u64().unwrap_or(0);
            let total = cursor["totalCharacters"].as_u64();
            let coverage = json!({"resultId":result_id,"processedCharacters":processed,"totalCharacters":total,
                "unit":"utf16-returned-public-text","state":if total.is_some_and(|total|processed>=total){"covered"}else{"partial"},
                "semanticCorrectnessCertified":false});
            replace_result_items(&mut record["summaryCoverage"], result_id, vec![coverage]);
            updates.insert(key, record);
        }
        let mut changed = 0;
        let mut information_added = false;
        for record in updates.into_values() {
            let previous = self.records.get(text(&record["sourceIdentity"]));
            // New receipt IDs and summary cursors are durable bookkeeping, not new source facts.
            // 新回执 ID 和摘要游标属于持久化元信息；只有原始读取覆盖或事实正文新增才算工作进展。
            information_added |= previous.is_none_or(|previous| {
                adds_read_coverage(previous, &record)
                    || array(&record["summaryFacts"]).iter().any(|fact| {
                        let body = text(&fact["text"]).trim();
                        !body.is_empty() && !array(&previous["summaryFacts"]).iter()
                            .any(|saved| text(&saved["text"]).trim() == body)
                    })
            });
            if self.persist_record(journal, record).await? {
                changed += 1;
            }
        }
        Ok(json!({"changedDocuments":changed,"informationAdded":information_added,"directory":self.directory()}))
    }

    /// Restore only registered artifacts in the current task tree; callers validate live source versions.
    /// 仅恢复当前任务树登记过的产物；调用方继续核对实时来源版本，不能把旧快照当新证据。
    pub async fn restore(journal: &TaskJournal, directory: &Value) -> Result<Self> {
        let mut restored = Self::default();
        for entry in array(&directory["documents"]) {
            let key = text(&entry["sourceIdentity"]);
            let reference = &entry["artifactRef"];
            let raw = journal.read_blob(reference).await?;
            let record: Value = serde_json::from_slice(&raw)
                .map_err(|_| invalid("Document work record is invalid. / 文档工作记录无效。"))?;
            if key.is_empty()
                || record["version"] != 1
                || record["sourceIdentity"] != key
                || reference["artifactId"]
                    .as_str()
                    .is_none_or(|id| !id.starts_with("document-progress-"))
            {
                return Err(invalid(
                    "Document work directory does not match its artifact. / 文档工作目录与产物不一致。",
                ));
            }
            restored.records.insert(key.to_owned(), record);
            restored
                .references
                .insert(key.to_owned(), reference.clone());
        }
        Ok(restored)
    }

    pub async fn read_record(&self, journal: &TaskJournal, source_identity: &str) -> Result<Value> {
        let reference = self
            .references
            .get(source_identity)
            .ok_or_else(|| invalid("Document work record was not found. / 未找到文档工作记录。"))?;
        let raw = journal.read_blob(reference).await?;
        serde_json::from_slice(&raw)
            .map_err(|_| invalid("Document work record is invalid. / 文档工作记录无效。"))
    }

    pub async fn invalidate_source(
        &mut self,
        journal: &TaskJournal,
        source_identity: &str,
        reason: &str,
    ) -> Result<()> {
        let mut record =
            self.records.get(source_identity).cloned().ok_or_else(|| {
                invalid("Document work record was not found. / 未找到文档工作记录。")
            })?;
        record["active"] = json!(false);
        record["invalidatedReason"] = json!(reason);
        self.persist_record(journal, record).await?;
        Ok(())
    }

    pub fn directory(&self) -> Value {
        json!({"version":1,"documents":self.records.keys().map(|key|self.entry(key)).collect::<Vec<_>>(),
            "semanticCorrectnessCertified":false})
    }

    pub fn compact_projection(&self) -> Value {
        let entries: Vec<Value> = self
            .records
            .iter()
            .filter(|(_, record)| record["active"] == true)
            .map(|(key, _)| self.entry(key))
            .collect();
        json!({"version":1,"documents":entries,"semanticCorrectnessCertified":false,
            "guidance":"Reuse saved analyses first. Read a document record for complete conclusions and original references; revisit originals only for invalidated versions or a specific evidence gap. / 优先复用已存分析；读取文档工作记录取得完整结论与原件引用，仅在版本失效或具体证据缺口时回读原文。"})
    }

    /// Plan only unobserved ranges of tracked output versions; dispatch still belongs to the ordinary tool pipeline.
    /// 只规划已跟踪产物当前版本的未读范围；真正派发仍走原工具执行链路、作用域校验与回执归档。
    pub fn missing_readback_plans(&self, targets: &Value, max_chars: u64) -> Vec<Value> {
        let mut plans = Vec::new();
        let limit = max_chars.clamp(1, 64_000);
        for target in array(targets) {
            let Some(path) = target["path"].as_str().filter(|path| !path.is_empty()) else { continue; };
            let Some(hash) = valid_hash(&target["sha256"]) else {
                if target["versionKnown"] == false {
                    plans.push(json!({"name":"document.read","arguments":{"path":path,"offset":0,"maxChars":limit},
                        "purpose":"discover-current-output-version","expectedSha256":Value::Null}));
                }
                continue;
            };
            if target["versionCheckRequired"] == true {
                plans.push(json!({"name":"filesystem.stat","arguments":{"path":path},
                    "purpose":"verify-current-output-version","expectedSha256":hash}));
                continue;
            }
            if target["readBackComplete"] == true { continue; }
            let matching: Vec<&Value> = self.records.values().filter(|record| {
                record["active"] == true && text(&record["source"]["path"]) == path &&
                    valid_hash(&record["sourceVersion"]["sha256"])
                        .is_some_and(|observed| observed.eq_ignore_ascii_case(hash))
            }).collect();
            let ranges: Vec<&Value> = matching.iter().flat_map(|record| array(&record["readRanges"]))
                .filter(|range| range["locator"]["extraction"]["complete"] != false).collect();
            if let Some(plan) = pdf_readback_plan(path, hash, &ranges, limit) {
                plans.push(plan);
                continue;
            }
            if ranges.iter().any(|range| range["locator"]["extraction"]["format"] == "pdf") {
                continue;
            }
            let total = target["totalCharacters"].as_u64().or_else(||
                ranges.iter().rev().find_map(|range| range["totalCharacters"].as_u64()));
            let mut spans: Vec<(u64, u64)> = array(&target["readRanges"]).iter().filter_map(|range|
                range.as_array().and_then(|range| range.first()?.as_u64().zip(range.get(1)?.as_u64())))
                .collect();
            spans.extend(ranges.iter().filter_map(|range| {
                (range["unit"] == "utf16" && range["totalCharacters"].as_u64() == total)
                    .then(|| range["start"].as_u64().zip(range["end"].as_u64())).flatten()
            }));
            let gap = total.map(|total| first_unread_span(total, &spans)).unwrap_or(Some((0, limit)));
            if let Some((start, end)) = gap.filter(|(start, end)| start < end && *start <= 33_554_432) {
                plans.push(readback_plan(path, hash, start, end, limit));
            }
        }
        plans
    }

    /// A trusted current file version invalidates old analysis without deleting the archived observations.
    /// 可信当前文件版本让旧分析立即失效；原始观察与历史归档保持完整。
    pub async fn observe_output_version(&mut self, journal: &TaskJournal, receipt: &Value) -> Result<bool> {
        let Some(path) = receipt["path"].as_str() else { return Ok(false); };
        let Some(hash) = valid_hash(&receipt["sha256"]).or_else(|| valid_hash(&receipt["version"])) else {
            return Ok(false);
        };
        let stale: Vec<String> = self.records.iter().filter(|(_, record)| record["active"] == true &&
            record["source"]["path"] == path && valid_hash(&record["sourceVersion"]["sha256"])
                .is_some_and(|previous| !previous.eq_ignore_ascii_case(hash)))
            .map(|(key, _)| key.clone()).collect();
        for key in &stale {
            self.invalidate_source(journal, key, "source-version-changed").await?;
        }
        Ok(!stale.is_empty())
    }

    /// Public findings are navigation for the current version, never a copy of the original or a completion certificate.
    /// 仅提供当前有效版本的公开成果导航，不复制原文、内部观察或声明任务完成。
    pub fn saved_findings(&self, source_identity: &str) -> Option<Value> {
        let record = self.records.get(source_identity)?;
        if record["active"] != true {
            return None;
        }
        let reference = self.references.get(source_identity)?;
        Some(json!({"sourceIdentity":source_identity,
            "conclusions":record["analysis"]["conclusions"],"unresolved":record["analysis"]["unresolved"],
            "summaryFacts":record["summaryFacts"],"artifactRef":reference,
            "semanticCorrectnessCertified":false}))
    }

    fn entry(&self, key: &str) -> Value {
        let record = &self.records[key];
        let analysis = &record["analysis"];
        let conclusions = array(&analysis["conclusions"]);
        let preview = conclusions
            .first()
            .and_then(Value::as_str)
            .or_else(|| {
                array(&record["summaryFacts"])
                    .first()
                    .and_then(|fact| fact["text"].as_str())
            })
            .unwrap_or("");
        json!({"sourceIdentity":key,"title":record["title"],"path":record["source"]["path"],
            "url":record["source"]["url"],"sourceId":record["source"]["sourceId"],
            "sourceVersion":record["sourceVersion"],"active":record["active"],"invalidatedReason":record["invalidatedReason"],
            "readState":read_state(record),"readRanges":record["readRanges"],
            "analysisState":if analysis["state"]=="recorded"{"recorded-unverified"}else if !array(&record["summaryFacts"]).is_empty(){"summary-facts-recorded"}else{"not-recorded"},
            "conclusionCount":conclusions.len(),"summaryFactCount":array(&record["summaryFacts"]).len(),
            "matchingReadCount":matching_read_count(record),
            "previewConclusion":preview.chars().take(256).collect::<String>(),
            "unresolved":analysis["unresolved"],"outputReferences":analysis["outputReferences"],
            "resultIds":array(&record["observations"]).iter().filter_map(|source|source["resultRef"]["id"].as_str()).collect::<Vec<_>>(),
            "artifactRef":self.references.get(key),"readBack":{"tool":"task.document","arguments":{"action":"read","sourceIdentity":key}},
            "semanticCorrectnessCertified":false})
    }

    fn observed_record(&self, tool: &str, source: &Value, result_ref: &Value) -> Result<Value> {
        let (target, version, key) = document_identity(source, result_ref)?;
        let mut record = self.records.get(&key).cloned().unwrap_or_else(||json!({
            "version":1,"sourceIdentity":key,"sourceTarget":target,"sourceVersion":version,"source":source,
            "title":source["title"].as_str().filter(|title|!title.is_empty()).unwrap_or_else(||source["path"].as_str().or_else(||source["url"].as_str()).or_else(||source["sourceId"].as_str()).unwrap_or("Document / 文档")),
            "active":true,"readRanges":[],"observations":[],"summaryFacts":[],"summaryCoverage":[],
            "analysis":{"state":"not-recorded","conclusions":[],"unresolved":[],"unresolvedStatus":"unknown","outputReferences":[],"semanticCorrectnessCertified":false},
            "semanticCorrectnessCertified":false
        }));
        let observation = json!({"tool":tool,"resultRef":result_ref,"source":source});
        insert_unique(&mut record["observations"], observation);
        let range = source_range(source);
        if range.is_object() {
            insert_unique(&mut record["readRanges"], range);
        }
        Ok(record)
    }

    async fn persist_record(&mut self, journal: &TaskJournal, record: Value) -> Result<bool> {
        let key = text(&record["sourceIdentity"]).to_owned();
        if self.records.get(&key) == Some(&record) && self.references.contains_key(&key) {
            return Ok(false);
        }
        let artifact = journal.save_task_blob("document-progress", &record).await?;
        self.references.insert(key.clone(), artifact.reference);
        self.records.insert(key, record);
        Ok(true)
    }

    async fn invalidate_superseded(&mut self, journal: &TaskJournal, newer: &Value) -> Result<()> {
        let old_keys: Vec<String> = self
            .records
            .iter()
            .filter(|(_, record)| {
                record["active"] == true
                    && record["sourceTarget"] == newer["sourceTarget"]
                    && record["sourceIdentity"] != newer["sourceIdentity"]
            })
            .map(|(key, _)| key.clone())
            .collect();
        for key in old_keys {
            self.invalidate_source(journal, &key, "source-version-changed")
                .await?;
        }
        Ok(())
    }
}

fn text(value: &Value) -> &str {
    value.as_str().unwrap_or("")
}
fn array(value: &Value) -> &[Value] {
    value.as_array().map(Vec::as_slice).unwrap_or(&[])
}

fn valid_hash(value: &Value) -> Option<&str> {
    value.as_str().filter(|hash| hash.len() == 64 && hash.bytes().all(|byte| byte.is_ascii_hexdigit()))
}

fn first_unread_span(total: u64, spans: &[(u64, u64)]) -> Option<(u64, u64)> {
    let mut sorted: Vec<(u64, u64)> = spans.iter().copied()
        .filter(|(start, end)| start <= end && *end <= total).collect();
    sorted.sort_unstable();
    let mut covered = 0;
    for (start, end) in sorted {
        if start > covered { return Some((covered, start)); }
        covered = covered.max(end);
    }
    (covered < total).then_some((covered, total))
}

fn readback_plan(path: &str, hash: &str, start: u64, end: u64, limit: u64) -> Value {
    json!({"name":"document.read","arguments":{"path":path,"offset":start,"maxChars":limit.min(end - start)},
        "purpose":"complete-current-output-readback","expectedSha256":hash,
        "missingRange":{"unit":"utf16","start":start,"end":end}})
}

fn pdf_readback_plan(path: &str, hash: &str, ranges: &[&Value], limit: u64) -> Option<Value> {
    let page_count = ranges.iter().find_map(|range| {
        let extraction = &range["locator"]["extraction"];
        (extraction["format"] == "pdf").then(|| extraction["pageCount"].as_u64()).flatten()
    })?;
    let mut windows: BTreeMap<(u64, u64, u64), Vec<(u64, u64)>> = BTreeMap::new();
    for range in ranges {
        let extraction = &range["locator"]["extraction"];
        if extraction["format"] != "pdf" || extraction["pageCount"].as_u64() != Some(page_count) { continue; }
        let window = &extraction["pageWindow"];
        if let Some(((first, last), total)) = window["startPage"].as_u64().zip(window["endPage"].as_u64())
            .zip(range["totalCharacters"].as_u64()).filter(|((first, last), _)| *first > 0 && first <= last && *last <= page_count)
            && let Some(span) = range["start"].as_u64().zip(range["end"].as_u64()) {
            windows.entry((first, last, total)).or_default().push(span);
        }
    }
    let mut covered_pages = Vec::new();
    for (&(first, last, total), spans) in &windows {
        if let Some((start, end)) = first_unread_span(total, spans) {
            if start > 33_554_432 { continue; }
            let mut plan = readback_plan(path, hash, start, end, limit);
            plan["arguments"]["startPage"] = json!(first);
            plan["arguments"]["endPage"] = json!(last);
            return Some(plan);
        }
        covered_pages.push((first - 1, last));
    }
    let (first, last) = first_unread_span(page_count, &covered_pages)?;
    if first >= 1_000_000 { return None; }
    let end_page = (first + 100).min(last).min(1_000_000);
    Some(json!({"name":"document.read","arguments":{"path":path,"offset":0,"maxChars":limit,
        "startPage":first + 1,"endPage":end_page},"purpose":"complete-current-output-readback",
        "expectedSha256":hash,"missingRange":{"unit":"pages","start":first + 1,"end":last}}))
}
fn invalid(message: &str) -> GatewayError {
    GatewayError::new("INVALID_DOCUMENT_PROGRESS", message, 409)
}

fn has_document_locator(source: &Value) -> bool {
    ["sourceId", "path", "url"]
        .iter()
        .any(|key| !text(&source[*key]).is_empty())
}

fn document_identity(source: &Value, result_ref: &Value) -> Result<(Value, Value, String)> {
    let id = text(&result_ref["id"]);
    if id.is_empty() {
        return Err(invalid(
            "Original read reference is required. / 缺少原始读取引用。",
        ));
    }
    let (kind, target) = ["sourceId", "path", "url"]
        .into_iter()
        .find_map(|key| (!text(&source[key]).is_empty()).then_some((key, source[key].clone())))
        .unwrap_or(("resultId", json!(id)));
    let mut version = serde_json::Map::new();
    for field in [
        "sha256",
        "contentHash",
        "sourceRevision",
        "scopeKey",
        "bindingRevision",
    ] {
        if let Some(value) = source.get(field).filter(|value| !value.is_null()) {
            version.insert(field.into(), value.clone());
        }
    }
    // Unversioned observations must never silently inherit another read's analysis.
    // 未取得真实内容版本时按原回执隔离，不能悄悄复用另一次读取的分析。
    if !version.contains_key("sha256")
        && !version.contains_key("contentHash")
        && !version.contains_key("sourceRevision")
    {
        version.insert("unversionedResultId".into(), json!(id));
    }
    let target = json!({"kind":kind,"value":target,"scopeKey":source["scopeKey"],"bindingRevision":source["bindingRevision"]});
    let version = Value::Object(version);
    let key = format!(
        "document-{}",
        managed_files::digest(json!([target, version]).to_string().as_bytes())
    );
    Ok((target, version, key))
}

fn source_range(source: &Value) -> Value {
    let mut range = serde_json::Map::new();
    if let Some((start, end)) = source["offset"]
        .as_u64()
        .zip(source["nextOffset"].as_u64())
        .filter(|(start, end)| start <= end)
    {
        range.insert("unit".into(), json!("utf16"));
        range.insert("start".into(), json!(start));
        range.insert("end".into(), json!(end));
        range.insert("totalCharacters".into(), source["totalCharacters"].clone());
    }
    for field in ["lineRange", "locator"] {
        if let Some(value) = source.get(field).filter(|value| !value.is_null()) {
            range.insert(field.into(), value.clone());
        }
    }
    if range.is_empty() {
        Value::Null
    } else {
        Value::Object(range)
    }
}

/// Count actual archived receipts for the latest tool and returned range within one document-version record.
/// 在同一文档版本中按最后一次读取的工具和真实返回范围计数；意图说明不参与，不把回执重放当新读取。
fn matching_read_count(record: &Value) -> usize {
    let observations = array(&record["observations"]);
    let Some(latest) = observations.last() else {
        return 0;
    };
    let range = source_range(&latest["source"]);
    if !range.is_object() {
        return 0;
    }
    observations
        .iter()
        .filter(|observation| {
            observation["tool"] == latest["tool"]
                && source_range(&observation["source"]) == range
        })
        .count()
}

fn read_state(record: &Value) -> &'static str {
    let ranges = array(&record["readRanges"]);
    if let Some(page_count) = ranges.iter().find_map(|range| {
        let extraction = &range["locator"]["extraction"];
        (extraction["format"] == "pdf").then(|| extraction["pageCount"].as_u64()).flatten()
    }) {
        // Text offsets restart within each PDF window; only fully read windows cover original pages.
        // PDF 各页窗口的字符偏移从头计数；只有正文已读完整的窗口才计入原文页覆盖。
        let mut windows: BTreeMap<(u64, u64), Vec<Value>> = BTreeMap::new();
        for range in ranges {
            let extraction = &range["locator"]["extraction"];
            if extraction["format"] != "pdf" || extraction["pageCount"].as_u64() != Some(page_count) {
                continue;
            }
            let window = &extraction["pageWindow"];
            if let Some((start, end)) = window["startPage"].as_u64().zip(window["endPage"].as_u64()) {
                let mut text_range = range.clone();
                text_range.as_object_mut().unwrap().remove("locator");
                windows.entry((start, end)).or_default().push(text_range);
            }
        }
        let mut covered_page = 0;
        for ((start, end), text_ranges) in windows {
            if read_state(&json!({"readRanges":text_ranges})) != "complete" {
                continue;
            }
            if start > covered_page + 1 {
                return "partial";
            }
            covered_page = covered_page.max(end);
        }
        return if covered_page >= page_count { "complete" } else { "partial" };
    }
    let mut spans: Vec<(u64, u64, u64)> = ranges
        .iter()
        .filter_map(|range| {
            Some((
                range["start"].as_u64()?,
                range["end"].as_u64()?,
                range["totalCharacters"].as_u64()?,
            ))
        })
        .collect();
    spans.sort_unstable();
    let mut end = 0;
    let Some(total) = spans.first().map(|span| span.2) else {
        return if ranges.is_empty() {
            "observed"
        } else {
            "partial"
        };
    };
    for (start, next, length) in spans {
        if start > end || length != total {
            return "partial";
        }
        end = end.max(next);
    }
    if end >= total { "complete" } else { "partial" }
}

fn adds_read_coverage(previous: &Value, current: &Value) -> bool {
    let old_ranges = array(&previous["readRanges"]);
    for range in array(&current["readRanges"]) {
        if old_ranges.contains(range) {
            continue;
        }
        let Some((start, end)) = range["start"].as_u64().zip(range["end"].as_u64()) else {
            return true;
        };
        let mut spans: Vec<(u64, u64)> = old_ranges
            .iter()
            .filter_map(|old| {
                (old["unit"] == range["unit"]
                    && old["locator"]["extraction"]["pageWindow"] == range["locator"]["extraction"]["pageWindow"])
                    .then(|| old["start"].as_u64().zip(old["end"].as_u64()))
                    .flatten()
            })
            .collect();
        spans.sort_unstable();
        let mut covered = start;
        for (old_start, old_end) in spans {
            if old_start <= covered {
                covered = covered.max(old_end);
            }
            if covered >= end {
                break;
            }
        }
        if covered < end {
            return true;
        }
    }
    false
}

fn insert_unique(target: &mut Value, value: Value) {
    if !target.is_array() {
        *target = json!([]);
    }
    let array = target.as_array_mut().unwrap();
    if !array.contains(&value) {
        array.push(value);
    }
}

fn replace_result_items(target: &mut Value, result_id: &str, values: Vec<Value>) {
    if !target.is_array() {
        *target = json!([]);
    }
    let target = target.as_array_mut().unwrap();
    target.retain(|value| value["resultId"].as_str() != Some(result_id));
    target.extend(values);
}

fn merge_observations(mut earlier: Value, newer: &Value) -> Value {
    for field in ["observations", "readRanges"] {
        for value in array(&newer[field]) {
            insert_unique(&mut earlier[field], value.clone());
        }
    }
    earlier
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::{sqlite_connections, task_repository::*};
    use std::path::PathBuf;

    #[test]
    fn document_readback_gap_plans_fill_docx_head_and_xlsx_tail_without_repeating_ranges() {
        let mut records = DocumentWorkRecords::default();
        let hash = "a".repeat(64);
        let source = json!({"path":"/work/guide.docx","sha256":hash,"offset":3600,"nextOffset":6008,"totalCharacters":6008});
        let record = records.observed_record("document.read", &source, &reference()).unwrap();
        records.records.insert(text(&record["sourceIdentity"]).to_owned(), record);
        let targets = json!([
            {"path":"/work/guide.docx","sha256":hash,"totalCharacters":6008},
            {"path":"/work/check.xlsx","sha256":hash,"totalCharacters":3731,"readRanges":[[0,3000]]}
        ]);
        let plans = records.missing_readback_plans(&targets, 12000);
        assert_eq!(plans.len(), 2);
        assert_eq!(plans[0]["arguments"], json!({"path":"/work/guide.docx","offset":0,"maxChars":3600}));
        assert_eq!(plans[1]["arguments"], json!({"path":"/work/check.xlsx","offset":3000,"maxChars":731}));
        let small = records.missing_readback_plans(&targets, 1000);
        assert_eq!(small[0]["arguments"]["maxChars"], 1000);
        let mut covered = targets;
        covered[0]["readRanges"] = json!([[0,3600]]);
        covered[1]["readRanges"] = json!([[0,3731]]);
        assert!(records.missing_readback_plans(&covered, 12000).is_empty());
    }

    #[test]
    fn document_readback_gap_plans_require_current_hash_and_never_accept_recorded_analysis_as_reading() {
        let mut records = DocumentWorkRecords::default();
        let source = json!({"path":"/work/deck.pptx","sha256":"a".repeat(64),"offset":0,"nextOffset":100,"totalCharacters":100});
        let mut old = records.observed_record("document.read", &source, &reference()).unwrap();
        old["analysis"] = json!({"state":"recorded","conclusions":["model says complete"]});
        records.records.insert(text(&old["sourceIdentity"]).to_owned(), old);
        let mut targets = json!([{"path":"/work/deck.pptx","sha256":"b".repeat(64)}]);
        let plans = records.missing_readback_plans(&targets, 12000);
        assert_eq!(plans[0]["arguments"], json!({"path":"/work/deck.pptx","offset":0,"maxChars":12000}));
        assert_eq!(plans[0]["expectedSha256"], "b".repeat(64));
        targets[0]["versionCheckRequired"] = json!(true);
        let plans = records.missing_readback_plans(&targets, 12000);
        assert_eq!(plans[0]["name"], "filesystem.stat");
        assert_eq!(plans[0]["arguments"], json!({"path":"/work/deck.pptx"}));
        targets[0]["sha256"] = Value::Null;
        assert!(records.missing_readback_plans(&targets, 12000).is_empty());
        targets[0]["versionKnown"] = json!(false);
        let fallback = records.missing_readback_plans(&targets, 12000);
        assert_eq!(fallback[0]["name"], "document.read");
        assert_eq!(fallback[0]["expectedSha256"], Value::Null);
    }

    #[test]
    fn document_page_windows_complete_only_after_all_pages_and_text_are_read() {
        let range = |first, last, start, end| json!({
            "unit":"utf16","start":start,"end":end,"totalCharacters":40,
            "locator":{"extraction":{"format":"pdf","pageCount":101,
                "pageWindow":{"startPage":first,"endPage":last}}}
        });
        let first = range(1, 100, 0, 40);
        let last_head = range(101, 101, 0, 20);
        let last_tail = range(101, 101, 20, 40);
        assert_eq!(read_state(&json!({"readRanges":[first]})), "partial");
        assert_eq!(read_state(&json!({"readRanges":[first,last_head]})), "partial");
        assert_eq!(read_state(&json!({"readRanges":[first,last_head,last_tail]})), "complete");
        assert!(adds_read_coverage(&json!({"readRanges":[first]}), &json!({"readRanges":[last_head]})));
    }

    struct Fixture {
        root: PathBuf,
    }
    impl Fixture {
        fn new() -> Self {
            let root = std::env::temp_dir()
                .join(format!("kynxa-document-progress-{}", uuid::Uuid::new_v4()));
            std::fs::create_dir_all(&root).unwrap();
            Self { root }
        }
    }
    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = sqlite_connections::close(&self.root, &self.root.join("Agent/tasks.sqlite"));
            let _ = std::fs::remove_dir_all(&self.root);
        }
    }
    fn input(request: &str, binding: u64) -> NewTask {
        NewTask {
            request_id: request.into(),
            goal: "Analyze two authorized documents".into(),
            acceptance: vec![],
            scope: TaskScope {
                project_id: Some("527b243e-5b7a-47e4-be73-4653a3b610c5".into()),
                conversation_id: "e746104c-85d9-41be-bf22-f1b69636cf6b".into(),
                permission_snapshot: json!({"mode":"work","bindingRevision":binding}),
            },
            parent_task_id: None,
            dependency_ids: vec![],
            budget: TaskBudget::default(),
            input_reference: None,
        }
    }
    fn receipt(hash: &str, start: u64, end: u64) -> Value {
        json!({"path":"/work/release.md","sha256":hash.repeat(64),"offset":start,"nextOffset":end,"totalCharacters":100})
    }
    fn reference() -> Value {
        json!({"id":uuid::Uuid::new_v4().to_string(),"sha256":"c".repeat(64),"bytes":512})
    }

    #[tokio::test]
    async fn summary_receipt_ids_and_cursors_do_not_reset_stagnation_without_new_facts() {
        use super::super::gateway_work_progress::{RoundProgress, WorkProgress};
        let fixture = Fixture::new();
        let journal = TaskJournal::begin(&fixture.root, &input("summary-progress", 1), "fixture", 60_000).unwrap();
        let mut records = DocumentWorkRecords::default();
        let original = reference();
        records.observe_read(&journal, "filesystem.read", &receipt("a", 0, 60), &original).await.unwrap();
        let mut summary = json!({"facts":[{"resultId":original["id"],"text":"The verified capacity is 138."}],
            "sources":[{"tool":"filesystem.read","resultRef":original,"source":receipt("a",0,60),"processedCharacters":20,"totalCharacters":60}]});
        let first = records.update_from_summary(&journal, &summary).await.unwrap();
        assert_eq!(first["informationAdded"], true);
        let mut progress = WorkProgress::default();
        for index in 1..=3 {
            let alias = reference();
            summary["facts"][0]["resultId"] = alias["id"].clone();
            summary["sources"][0]["resultRef"] = alias;
            summary["sources"][0]["processedCharacters"] = json!(20 + index);
            let updated = records.update_from_summary(&journal, &summary).await.unwrap();
            assert!(updated["changedDocuments"].as_u64().unwrap() > 0);
            assert_eq!(updated["informationAdded"], false);
            let plan = progress.observe_round(&RoundProgress {
                evidence_changed: updated["informationAdded"] == true,
                source_reads:1, repeated_reads:1, ..Default::default()
            });
            assert_eq!(plan.no_progress_rounds, index);
        }
        assert!(progress.needs_strategy_change());
        summary["facts"][0]["text"] = json!("The capacity applies to production only.");
        assert_eq!(records.update_from_summary(&journal, &summary).await.unwrap()["informationAdded"], true);
    }

    #[tokio::test]
    async fn document_analyses_restore_without_originals_and_new_versions_invalidate_old_work() {
        let fixture = Fixture::new();
        let journal = TaskJournal::begin(
            &fixture.root,
            &input("document-records", 1),
            "fixture",
            60_000,
        )
        .unwrap();
        let mut records = DocumentWorkRecords::default();
        let first_ref = reference();
        let first = records
            .observe_read(
                &journal,
                "filesystem.read",
                &receipt("a", 0, 60),
                &first_ref,
            )
            .await
            .unwrap()
            .unwrap();
        let key = text(&first["sourceIdentity"]).to_owned();
        assert_eq!(first["readState"], "partial");
        assert_eq!(first["analysisState"], "not-recorded");
        assert_eq!(first["informationAdded"], true);
        assert_eq!(first["matchingReadCount"], 1);
        let artifact_count = journal
            .repository()
            .artifacts(journal.task_id())
            .unwrap()
            .len();
        records
            .observe_read(
                &journal,
                "filesystem.read",
                &receipt("a", 0, 60),
                &first_ref,
            )
            .await
            .unwrap();
        assert_eq!(
            journal
                .repository()
                .artifacts(journal.task_id())
                .unwrap()
                .len(),
            artifact_count
        );
        let alias = records
            .observe_read(
                &journal,
                "filesystem.read",
                &receipt("a", 0, 60),
                &reference(),
            )
            .await
            .unwrap()
            .unwrap();
        assert_eq!(alias["informationAdded"], false);
        assert_eq!(alias["matchingReadCount"], 2);
        records
            .record_analysis(
                &journal,
                text(&first_ref["id"]),
                &["Final operative capacity is 138.".into()],
                &["Need the applicability condition in the last range.".into()],
                &[],
            )
            .await
            .unwrap();
        let same_analysis = records
            .record_analysis(
                &journal,
                text(&first_ref["id"]),
                &["Final operative capacity is 138.".into()],
                &["Need the applicability condition in the last range.".into()],
                &[],
            )
            .await
            .unwrap();
        assert_eq!(same_analysis["informationAdded"], false);
        let second_ref = reference();
        let second = records
            .observe_read(
                &journal,
                "filesystem.read",
                &receipt("a", 60, 100),
                &second_ref,
            )
            .await
            .unwrap()
            .unwrap();
        assert_eq!(second["sourceIdentity"], key);
        assert_eq!(second["readState"], "complete");
        assert_eq!(second["informationAdded"], true);
        assert_eq!(second["matchingReadCount"], 1);
        let overlap = records
            .observe_read(
                &journal,
                "filesystem.read",
                &receipt("a", 20, 40),
                &reference(),
            )
            .await
            .unwrap()
            .unwrap();
        assert_eq!(overlap["informationAdded"], false);
        assert_eq!(overlap["matchingReadCount"], 1);
        let summary = json!({"facts":[{"resultId":second_ref["id"],"text":"Capacity applies to production only."}],
            "sources":[{"tool":"filesystem.read","resultRef":second_ref,"source":receipt("a",60,100),"processedCharacters":40,"totalCharacters":40}]});
        records
            .update_from_summary(&journal, &summary)
            .await
            .unwrap();
        let restored = DocumentWorkRecords::restore(&journal, &records.directory())
            .await
            .unwrap();
        let saved = restored.read_record(&journal, &key).await.unwrap();
        assert_eq!(
            saved["analysis"]["conclusions"][0],
            "Final operative capacity is 138."
        );
        assert_eq!(
            saved["summaryFacts"][0]["text"],
            "Capacity applies to production only."
        );
        assert_eq!(saved["semanticCorrectnessCertified"], false);
        let findings = restored.saved_findings(&key).unwrap();
        assert_eq!(findings["conclusions"], saved["analysis"]["conclusions"]);
        assert_eq!(findings["unresolved"], saved["analysis"]["unresolved"]);
        assert_eq!(findings["summaryFacts"], saved["summaryFacts"]);
        assert_eq!(findings["artifactRef"], restored.directory()["documents"][0]["artifactRef"]);
        assert!(findings["observations"].is_null());
        assert!(findings["source"].is_null());
        assert_eq!(
            restored.compact_projection()["documents"][0]["readBack"]["arguments"]["action"],
            "read"
        );
        let newer = records
            .observe_read(
                &journal,
                "filesystem.read",
                &receipt("b", 0, 100),
                &reference(),
            )
            .await
            .unwrap()
            .unwrap();
        assert_ne!(newer["sourceIdentity"], key);
        assert_eq!(newer["matchingReadCount"], 1);
        assert!(records.saved_findings(&key).is_none());
        assert_eq!(
            records.read_record(&journal, &key).await.unwrap()["active"],
            false
        );
        records
            .update_from_summary(&journal, &summary)
            .await
            .unwrap();
        assert_eq!(
            records.compact_projection()["documents"]
                .as_array()
                .unwrap()
                .len(),
            1
        );
        assert_eq!(
            records.compact_projection()["documents"][0]["sourceIdentity"],
            newer["sourceIdentity"]
        );
    }

    #[tokio::test]
    async fn unobserved_analysis_and_other_task_scope_are_rejected() {
        let fixture = Fixture::new();
        let journal =
            TaskJournal::begin(&fixture.root, &input("owned-record", 1), "fixture", 60_000)
                .unwrap();
        let mut records = DocumentWorkRecords::default();
        assert!(
            records
                .record_analysis(&journal, "missing", &["Unobserved claim".into()], &[], &[])
                .await
                .is_err()
        );
        records
            .observe_read(
                &journal,
                "filesystem.read",
                &receipt("a", 0, 100),
                &reference(),
            )
            .await
            .unwrap();
        let outsider = TaskJournal::begin(
            &fixture.root,
            &input("outside-record", 2),
            "fixture",
            60_000,
        )
        .unwrap();
        assert!(
            DocumentWorkRecords::restore(&outsider, &records.directory())
                .await
                .is_err()
        );
    }
}

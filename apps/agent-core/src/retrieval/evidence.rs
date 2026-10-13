use std::collections::HashSet;

use serde_json::{Value, json};

#[derive(Default)]
struct OrderedRecords(Vec<(String, Value)>);
impl OrderedRecords {
    fn get(&self, key: &str) -> Option<&Value> {
        self.0
            .iter()
            .find(|(entry, _)| entry == key)
            .map(|(_, value)| value)
    }
    fn set(&mut self, key: String, value: Value) {
        if let Some((_, existing)) = self.0.iter_mut().find(|(entry, _)| entry == &key) {
            *existing = value;
        } else {
            self.0.push((key, value));
        }
    }
    fn remove(&mut self, key: &str) {
        self.0.retain(|(entry, _)| entry != key);
    }
    fn trim(&mut self, maximum: usize) -> Vec<String> {
        let count = self.0.len().saturating_sub(maximum);
        self.0.drain(..count).map(|(key, _)| key).collect()
    }
}
fn text<'a>(value: &'a Value, key: &str) -> &'a str {
    value[key].as_str().unwrap_or("")
}

/// Evidence decisions own only identities, versions and bounded facts; original quotes and complete tool receipts stay in Node.
/// 证据决策只持有身份、版本与有界事实；原始引用和完整工具回执继续由 Node 缓存与持久化。
#[derive(Default)]
pub struct EvidenceLedger {
    opened: bool,
    research: bool,
    searches: u64,
    gaps: OrderedRecords,
    seen: HashSet<String>,
    cached: OrderedRecords,
    reads: OrderedRecords,
    dependencies: OrderedRecords,
    versions: OrderedRecords,
    conclusions: OrderedRecords,
    strategies: OrderedRecords,
}

impl EvidenceLedger {
    fn maximum_searches(&self) -> u64 {
        if self.research { 48 } else { 16 }
    }
    fn maximum_per_gap(&self) -> u64 {
        if self.research { 8 } else { 4 }
    }
    fn summary(&self) -> Value {
        json!({"searches":self.searches,"maximumSearches":self.maximum_searches(),"maximumSearchesPerGap":self.maximum_per_gap(),"maximumUnproductiveSearches":if self.research {3} else {2}})
    }
    fn status(&self, ticket: &Value, args: &Value) -> Value {
        let blocked = args["blocked"] == true;
        let has = args["hasEvidence"] == true;
        let reused = args["reused"] == true;
        let gap_searches = self
            .gaps
            .get(text(ticket, "gapKey"))
            .and_then(Value::as_u64)
            .unwrap_or(0);
        json!({"state":if blocked {"search-budget-exhausted"} else if has {"evidence-available"} else {"no-evidence"},"sufficiency":"not-evaluated",
            "missingInformation":ticket["gap"],"explicitGap":ticket["explicitGap"],"newEvidenceCount":args.get("newEvidenceCount").unwrap_or(&json!(0)),"reused":reused,"executed":!reused && !blocked,
            "remainingSearches":self.maximum_searches().saturating_sub(self.searches),"remainingGapSearches":self.maximum_per_gap().saturating_sub(gap_searches),
            "next":if blocked {"answer-supported-parts-and-state-the-gap"} else if has {"answer-if-supported-otherwise-read-the-missing-section"} else {"search-a-specific-gap-or-state-it-is-unknown"},
            "notice":"Relevance is not sufficiency. Check the requested entity, time, scope and conditions against the evidence. Answer now when it supports the requested facts; otherwise describe the missing fact and read its section. Exhausted retrieval does not mean the answer is verified; file edits and tests may continue."})
    }
    fn invalidate_source(&mut self, key: &str, reason: Option<&str>) -> Vec<Value> {
        self.reads.remove(key);
        for (_, conclusion) in &mut self.conclusions.0 {
            if conclusion["sourceKeys"]
                .as_array()
                .is_some_and(|keys| keys.iter().any(|value| value.as_str() == Some(key)))
            {
                conclusion["state"] = json!("stale");
                if let Some(reason) = reason {
                    conclusion["reason"] = json!(reason);
                }
            }
        }
        self.conclusions
            .0
            .iter()
            .filter(|(_, conclusion)| conclusion["state"] == "stale")
            .map(|(_, conclusion)| conclusion["conclusionId"].clone())
            .collect()
    }
    fn observe(&mut self, args: &Value) -> Result<Value, String> {
        let ticket = &args["ticket"];
        let reused = args["reused"] == true;
        let items = args["items"]
            .as_array()
            .ok_or("INVALID_RETRIEVAL_EVIDENCE")?;
        if !reused {
            self.searches += 1;
            let key = text(ticket, "gapKey");
            let count = self.gaps.get(key).and_then(Value::as_u64).unwrap_or(0);
            self.gaps.set(key.to_owned(), json!(count + 1));
        }
        let mut new_evidence = 0;
        let mut invalidated = Vec::new();
        let mut drop_cached = Vec::new();
        let mut drop_reads = Vec::new();
        for item in items {
            let key = text(item, "sourceKey");
            let version = text(item, "version");
            if let Some(previous) = self
                .versions
                .get(key)
                .filter(|previous| previous["version"] != version)
            {
                invalidated.push(previous["sourceRef"].clone());
                let stale = self
                    .cached
                    .0
                    .iter()
                    .filter(|(_, cached)| {
                        cached["sourceKeys"].as_array().is_some_and(|keys| {
                            keys.iter().any(|value| value.as_str() == Some(key))
                        })
                    })
                    .map(|(key, _)| key.clone())
                    .collect::<Vec<_>>();
                for cache in stale {
                    self.cached.remove(&cache);
                    drop_cached.push(cache);
                }
                self.invalidate_source(key, Some("source-version-changed"));
                drop_reads.push(key.to_owned());
            }
            self.versions.set(
                key.to_owned(),
                json!({"version":version,"sourceRef":item["sourceRef"]}),
            );
            self.versions.trim(128);
            if self.seen.insert(text(item, "evidenceKey").to_owned()) {
                new_evidence += 1;
            }
        }
        if !reused && !items.is_empty() {
            self.cached.set(text(ticket,"key").to_owned(),json!({"sourceKeys":items.iter().map(|item|item["sourceKey"].clone()).collect::<Vec<_>>()}));
        }
        drop_cached.extend(self.cached.trim(32));
        let strategy = text(args, "strategyKey");
        let mut no_progress = self
            .strategies
            .get(strategy)
            .and_then(Value::as_u64)
            .unwrap_or(0);
        if !reused {
            no_progress = if new_evidence > 0 { 0 } else { no_progress + 1 };
            self.strategies.remove(strategy);
            self.strategies.set(strategy.to_owned(), json!(no_progress));
            self.strategies.trim(32);
        }
        let mut status=self.status(ticket,&json!({"hasEvidence":!items.is_empty(),"newEvidenceCount":new_evidence,"reused":reused}));
        status["noProgressSearches"] = json!(no_progress);
        status["stopSameStrategy"] =
            json!(reused || no_progress >= if self.research { 3 } else { 2 });
        Ok(
            json!({"status":status,"invalidatedSourceRefs":invalidated,"dropCachedKeys":drop_cached,"dropReadKeys":drop_reads,
            "entrySearches":self.gaps.get(text(ticket,"gapKey")),"cacheResult":!reused && !items.is_empty()}),
        )
    }
    fn projection(&mut self, args: &Value) -> Result<Value, String> {
        let items = args["items"]
            .as_array()
            .ok_or("INVALID_RETRIEVAL_EVIDENCE")?;
        let mut keys = HashSet::new();
        let additions = items
            .iter()
            .filter(|item| {
                self.dependencies.get(text(item, "sourceKey")).is_none()
                    && keys.insert(text(item, "sourceKey"))
            })
            .count();
        if self.dependencies.0.len() + additions > 1024 {
            return Err("RETRIEVAL_VERSION_LEDGER_LIMIT".into());
        }
        for item in items {
            self.dependencies.set(text(item,"sourceKey").to_owned(),json!({"sourceRef":item["sourceRef"],"sourceId":item["sourceId"],"scopeKey":item["scopeKey"],"version":item["version"]}));
        }
        Ok(Value::Null)
    }
    fn read(&mut self, args: &Value) -> Result<Value, String> {
        let item = &args["item"];
        let key = text(item, "sourceKey");
        let version = text(item, "version");
        if self.dependencies.get(key).is_none() && self.dependencies.0.len() >= 1024 {
            return Err("RETRIEVAL_VERSION_LEDGER_LIMIT".into());
        }
        let changed = self
            .versions
            .get(key)
            .is_some_and(|previous| previous["version"] != version);
        if changed {
            self.invalidate_source(key, None);
        }
        self.versions.set(
            key.to_owned(),
            json!({"version":version,"sourceRef":item["sourceRef"]}),
        );
        self.versions.trim(128);
        let previous = self
            .reads
            .get(key)
            .filter(|previous| previous["version"] == version);
        let mut observations = previous
            .and_then(|previous| previous["observations"].as_array())
            .cloned()
            .unwrap_or_default();
        let observation = &args["observation"];
        let repeated = observations.contains(observation);
        observations.retain(|previous| previous != observation);
        observations.push(observation.clone());
        let count = observations.len().saturating_sub(32);
        observations.drain(..count);
        let new_content = !repeated && args["textLength"].as_u64().unwrap_or(0) > 0;
        if new_content {
            self.strategies.0.clear();
        }
        let fragments = args["fragments"]
            .as_array()
            .ok_or("INVALID_RETRIEVAL_EVIDENCE")?;
        let mut start = 0;
        let mut total = fragments
            .iter()
            .map(|fragment| fragment["length"].as_u64().unwrap_or(0))
            .sum::<u64>();
        while fragments.len() - start > 16 || total > 131072 {
            total -= fragments[start]["length"].as_u64().unwrap_or(0);
            start += 1;
        }
        self.reads.set(key.to_owned(),json!({"version":version,"sourceRef":item["sourceRef"],"observations":observations,
            "references":fragments[start..].iter().map(|fragment|fragment["sourceRef"].clone()).collect::<Vec<_>>()}));
        self.dependencies.set(key.to_owned(),json!({"sourceRef":item["sourceRef"],"sourceId":item["sourceId"],"scopeKey":item["scopeKey"],"version":version}));
        let evicted = self.reads.trim(64);
        Ok(
            json!({"dropReadKeys":evicted,"fragmentStart":start,"versionChanged":changed,
            "observations":observations,
            "decision":{"sufficiency":"not-evaluated","missingInformation":args["gap"],"sourceVersionChecked":true,"contentRead":true,"repeated":repeated,"newContent":new_content,
                "range":args["range"],"next":if repeated {"use-existing-evidence-or-read-a-different-range"} else {"evaluate-support-then-answer-or-name-the-remaining-gap"}}}),
        )
    }
    fn assess(&mut self, args: &Value) -> Result<Value, String> {
        let input = &args["assessment"];
        let id = input
            .get("conclusionId")
            .map_or(Some("current"), Value::as_str)
            .ok_or("INVALID_RETRIEVAL_ASSESSMENT")?;
        if id.is_empty() || id.encode_utf16().count() > 128 {
            return Err("INVALID_RETRIEVAL_ASSESSMENT".into());
        }
        let claims = input["claims"]
            .as_array()
            .filter(|claims| !claims.is_empty() && claims.len() <= 32)
            .ok_or("INVALID_RETRIEVAL_ASSESSMENT")?;
        let empty = json!([]);
        let unresolved = input
            .get("unresolved")
            .unwrap_or(&empty)
            .as_array()
            .filter(|items| items.len() <= 32)
            .ok_or("INVALID_RETRIEVAL_ASSESSMENT")?;
        let contradictions = input
            .get("contradictions")
            .unwrap_or(&empty)
            .as_array()
            .filter(|items| items.len() <= 32)
            .ok_or("INVALID_RETRIEVAL_ASSESSMENT")?;
        let verification = input
            .get("verification")
            .unwrap_or(&empty)
            .as_array()
            .filter(|items| items.len() <= 32)
            .ok_or("INVALID_RETRIEVAL_ASSESSMENT")?;
        let mut evidence = Vec::new();
        let mut source_keys = Vec::new();
        for claim in claims {
            let statement = claim["statement"]
                .as_str()
                .filter(|statement| {
                    !statement.trim().is_empty() && statement.encode_utf16().count() <= 2000
                })
                .ok_or("INVALID_RETRIEVAL_ASSESSMENT")?;
            let citations = claim["support"]
                .as_array()
                .filter(|items| items.len() <= 16)
                .ok_or("INVALID_RETRIEVAL_ASSESSMENT")?;
            if claim.get("semanticSupport").is_some_and(|state| {
                !matches!(
                    state.as_str(),
                    Some("supports" | "partial" | "contradicts" | "uncertain")
                )
            }) {
                return Err("INVALID_RETRIEVAL_ASSESSMENT".into());
            }
            let mut supports = Vec::new();
            for citation in citations {
                let reference = citation["sourceRef"]
                    .as_str()
                    .ok_or("INVALID_RETRIEVAL_ASSESSMENT")?;
                let quotation = citation["quote"]
                    .as_str()
                    .filter(|quote| {
                        !quote.trim().is_empty() && quote.encode_utf16().count() <= 8000
                    })
                    .ok_or("INVALID_RETRIEVAL_ASSESSMENT")?;
                let _ = quotation;
                let read = self.reads.0.iter().find(|(_, read)| {
                    read["sourceRef"] == reference
                        || read["references"].as_array().is_some_and(|references| {
                            references.iter().any(|value| value == reference)
                        })
                });
                let version_current = read.is_some_and(|(key, read)| {
                    self.versions
                        .get(key)
                        .is_none_or(|current| current["version"] == read["version"])
                });
                let quote_present = read.is_some() && citation["quotePresent"] == true;
                if let Some((key, _)) = read {
                    if !source_keys.contains(key) {
                        source_keys.push(key.clone());
                    }
                }
                supports.push(json!({"sourceRef":reference,"contentRead":read.is_some(),"versionCurrent":version_current,"quotePresent":quote_present}));
            }
            let state = if supports.is_empty()
                || supports.iter().any(|support| {
                    support["versionCurrent"] != true || support["quotePresent"] != true
                }) {
                "missing-support"
            } else if claim
                .get("semanticSupport")
                .is_some_and(|support| support != "supports")
            {
                "semantic-support-unresolved"
            } else {
                "cited-source-read"
            };
            evidence.push(json!({"statement":statement,"support":supports,"semanticSupport":{"state":claim.get("semanticSupport").unwrap_or(&json!("not-evaluated")),"author":"model","correctnessCertified":false},"state":state}));
        }
        let checks=verification.iter().map(|check|json!({"name":check["name"],"state":"unverified","reason":"requires-execution-receipt","toolCallId":check.get("toolCallId").unwrap_or(&Value::Null)})).collect::<Vec<_>>();
        let ready = evidence
            .iter()
            .all(|claim| claim["state"] == "cited-source-read")
            && unresolved.is_empty()
            && contradictions.is_empty()
            && checks.is_empty();
        let conclusion = json!({"conclusionId":id,"sourceKeys":source_keys,"claims":evidence,"unresolved":unresolved,"contradictions":contradictions,"checks":checks,
            "state":if ready {"ready-to-answer"} else if !contradictions.is_empty() {"contradictory"} else {"needs-evidence-or-validation"},"sufficiency":"cited-support-checked","correctnessCertified":false,
            "next":if !contradictions.is_empty() {"resolve-conflicting-sources"} else if ready {"answer-with-citations"} else {"acquire-the-named-gap-or-run-validation"}});
        self.conclusions.set(id.to_owned(), conclusion.clone());
        self.conclusions.trim(32);
        Ok(conclusion)
    }
    pub fn apply(&mut self, operation: &str, args: &Value) -> Result<Value, String> {
        if operation == "retrieval.evidence-open" {
            if self.opened {
                return Err("AGENT_CORE_SESSION_EXISTS".into());
            }
            self.opened = true;
            self.research = args["research"] == true;
        } else if !self.opened {
            return Err("AGENT_CORE_INVALID_SESSION".into());
        }
        let result = match operation {
            "retrieval.evidence-open" => Value::Null,
            "retrieval.evidence-prepare" => {
                let gap_key = text(args, "gapKey");
                if self.gaps.get(gap_key).is_none() {
                    self.gaps.set(gap_key.to_owned(), json!(0));
                }
                let searches = self.gaps.get(gap_key).and_then(Value::as_u64).unwrap_or(0);
                let cached = self.cached.get(text(args, "key")).is_some();
                json!({"key":args["key"],"snapshotKey":args["snapshotKey"],"gapKey":gap_key,"gap":args["gap"],"explicitGap":args["explicitGap"],"entrySearches":searches,"hasCached":cached,
                    "blocked":!cached && (self.searches>=self.maximum_searches() || searches>=self.maximum_per_gap())})
            }
            "retrieval.evidence-invalidate" => {
                self.cached.remove(text(args, "key"));
                Value::Null
            }
            "retrieval.evidence-observe" => self.observe(args)?,
            "retrieval.evidence-projection" => self.projection(args)?,
            "retrieval.evidence-read" => self.read(args)?,
            "retrieval.evidence-assess" => self.assess(args)?,
            "retrieval.evidence-invalidate-source" => {
                json!(self.invalidate_source(text(args, "sourceKey"), None))
            }
            "retrieval.evidence-status" => self.status(&args["ticket"], args),
            "retrieval.evidence-state" => {
                json!({"gaps":self.gaps.0,"versions":self.versions.0,"dependencies":self.dependencies.0,"conclusions":self.conclusions.0})
            }
            "retrieval.evidence-progress-decision" => {
                let mut decision = args["decision"].clone();
                if args["progress"]["stopSameStrategy"] == true
                    && !matches!(
                        decision["next"].as_str(),
                        Some("read-source" | "state-unresolved-gap")
                    )
                {
                    decision["shouldContinueSearch"] = json!(false);
                    decision["next"] = json!("switch-channel-or-read-known-source");
                    decision["stopReason"] = json!("no-new-versioned-evidence");
                    decision["suggestedOperations"] = json!([
                        "source-read",
                        "symbol-search",
                        "path-search",
                        "relation-lookup"
                    ]);
                    decision["sufficiency"] = json!("not-evaluated");
                }
                decision
            }
            _ => return Err("AGENT_CORE_UNKNOWN_OPERATION".into()),
        };
        Ok(json!({"result":result,"metrics":self.summary()}))
    }
}

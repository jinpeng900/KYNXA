use serde_json::{Value, json};

/// Publication admission happens before I/O and cannot reopen after a committed archive.
/// 发布准入发生在 I/O 前；归档已经提交后不能重新开放句柄并重复发布。
#[derive(Default)]
pub struct EvidencePublicationState {
    context_key: String,
    maximum_tokens: u64,
    maximum_characters: u64,
    finalizing: bool,
    published: bool,
}

impl EvidencePublicationState {
    pub fn apply(&mut self, operation: &str, args: &Value) -> Result<Value, String> {
        match operation {
            "orchestration.publication.open" => {
                self.context_key = args["contextKey"]
                    .as_str()
                    .ok_or("RETRIEVAL_PREPARATION_INVALID")?
                    .into();
                self.maximum_tokens = args["maximumTokens"]
                    .as_u64()
                    .ok_or("INVALID_RETRIEVAL_INPUT")?;
                self.maximum_characters = args["maximumCharacters"]
                    .as_u64()
                    .ok_or("INVALID_RETRIEVAL_INPUT")?;
                Ok(Value::Null)
            }
            "orchestration.publication.admit" => {
                if self.published || self.finalizing || args["contextKey"] != self.context_key {
                    return Err("RETRIEVAL_PREPARATION_INVALID".into());
                }
                let token_budget = args
                    .get("maximumTokens")
                    .filter(|v| !v.is_null())
                    .map(|v| v.as_u64().ok_or("INVALID_RETRIEVAL_INPUT"))
                    .transpose()?
                    .unwrap_or(self.maximum_tokens);
                let character_budget = args
                    .get("maximumCharacters")
                    .filter(|v| !v.is_null())
                    .map(|v| v.as_u64().ok_or("INVALID_RETRIEVAL_INPUT"))
                    .transpose()?
                    .unwrap_or(self.maximum_characters);
                self.finalizing = true;
                Ok(
                    json!({"tokenBudget":token_budget.min(self.maximum_tokens),"characterBudget":character_budget.min(self.maximum_characters)}),
                )
            }
            "orchestration.publication.committed" => {
                self.published = true;
                Ok(Value::Null)
            }
            "orchestration.publication.failed" => {
                if !self.published {
                    self.finalizing = false;
                }
                Ok(Value::Null)
            }
            _ => Err("AGENT_CORE_UNKNOWN_OPERATION".into()),
        }
    }
}

pub fn projection_audit(args: &Value) -> Value {
    let previous = &args["previous"];
    let projection = &args["projection"];
    let mut projections = previous["projections"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    let mut entry = projection["audit"].as_object().cloned().unwrap_or_default();
    entry.insert("phase".into(), args["phase"].clone());
    entry.insert("requested".into(), args["requested"].clone());
    projections.push(Value::Object(entry));
    let mut actual = previous["actual"].as_object().cloned().unwrap_or_default();
    actual.insert("projectedCount".into(), args["projectedCount"].clone());
    actual.insert("promptTokens".into(), projection["usedTokens"].clone());
    actual.insert("promptCharacters".into(), args["promptCharacters"].clone());
    let mut reasons = previous["earlyCutReasons"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    for reason in projection["audit"]["earlyCutReasons"]
        .as_array()
        .into_iter()
        .flatten()
    {
        let mut reason = reason.clone();
        reason["phase"] = args["phase"].clone();
        reasons.push(reason);
    }
    let mut audit = previous.as_object().cloned().unwrap_or_default();
    audit.insert("projections".into(), json!(projections));
    audit.insert("actual".into(), Value::Object(actual));
    audit.insert("earlyCutReasons".into(), json!(reasons));
    Value::Object(audit)
}

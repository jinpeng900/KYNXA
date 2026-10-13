use std::collections::BTreeSet;

use serde_json::{Value, json};

/// Accepted lookahead and publication ownership outlive consumer cancellation until actual I/O settles.
/// 已接纳预取及发布的所有权独立于消费者取消；只有真实 I/O 结算才释放，不承诺未落盘的批次。
#[derive(Default)]
pub struct SourcePipelineState {
    count: u64,
    concurrency: u64,
    next: u64,
    closed: bool,
    pending: BTreeSet<u64>,
    publication_sequence: u64,
    publication: Option<u64>,
    draining: BTreeSet<u64>,
}
impl SourcePipelineState {
    pub fn apply(&mut self, operation: &str, args: &Value) -> Result<Value, String> {
        match operation {
            "orchestration.sourceManagement.pipeline.open" => {
                self.concurrency = args["concurrency"]
                    .as_u64()
                    .filter(|value| (1..=4).contains(value))
                    .ok_or("INVALID_SOURCE_PREPARATION_CONCURRENCY")?;
                self.count = args["count"]
                    .as_u64()
                    .ok_or("INVALID_SOURCE_PREPARATION_COUNT")?;
                Ok(Value::Null)
            }
            "orchestration.sourceManagement.pipeline.plan" => {
                let mut positions = Vec::new();
                while !self.closed
                    && args["aborted"] != true
                    && self.next < self.count
                    && self.pending.len() < self.concurrency as usize
                {
                    self.pending.insert(self.next);
                    positions.push(self.next);
                    self.next += 1;
                }
                Ok(json!(positions))
            }
            "orchestration.sourceManagement.pipeline.consumed" => {
                if !self.pending.remove(
                    &args["position"]
                        .as_u64()
                        .ok_or("INVALID_SOURCE_PREPARATION_POSITION")?,
                ) {
                    return Err("SOURCE_PREPARATION_NOT_OWNED".into());
                }
                Ok(Value::Null)
            }
            "orchestration.sourceManagement.pipeline.close" => {
                self.closed = true;
                Ok(json!(self.pending.iter().copied().collect::<Vec<_>>()))
            }
            "orchestration.sourceManagement.pipeline.drained" => {
                self.pending.clear();
                Ok(Value::Null)
            }
            "orchestration.sourceManagement.publication.submit" => {
                if self.publication.is_some() || !self.draining.is_empty() {
                    return Err("INDEX_PUBLICATION_OUTSTANDING".into());
                }
                self.publication_sequence += 1;
                self.publication = Some(self.publication_sequence);
                Ok(json!(self.publication_sequence))
            }
            "orchestration.sourceManagement.publication.drain" => {
                let id = self.publication.take();
                if let Some(id) = id {
                    self.draining.insert(id);
                }
                Ok(json!(id))
            }
            "orchestration.sourceManagement.publication.settled" => {
                let id = args["id"]
                    .as_u64()
                    .ok_or("INVALID_INDEX_PUBLICATION_RECEIPT")?;
                if !self.draining.remove(&id) {
                    return Err("INDEX_PUBLICATION_NOT_OWNED".into());
                }
                Ok(json!({"failed": args["failed"] == true}))
            }
            _ => Err("AGENT_CORE_UNKNOWN_OPERATION".into()),
        }
    }
}

//! Rebuild only already committed public stream observations, never model/tool execution.
//! 只重建已经提交的公开流观察，不恢复或重复模型、工具执行。

use super::TaskEvent;
use serde_json::{Value, json};
use std::collections::HashMap;

const MAXIMUM_PROJECTION_BYTES: usize = 128 * 1024 * 1024;

/// Text stays in mutable buffers until final projection; deltas never clone the accumulated segment.
/// 正文在最终投影前保留可追加缓冲；增量不会反复复制已经累计的整段文字。
#[derive(Default)]
struct SegmentBuffer {
    metadata: serde_json::Map<String, Value>,
    content: String,
    reasoning: String,
    base_content: String,
    base_reasoning: String,
    base_known: bool,
}

impl SegmentBuffer {
    fn project(self) -> Value {
        let mut value = Value::Object(self.metadata);
        value["contentAppend"] = json!(self.content);
        value["reasoningAppend"] = json!(self.reasoning);
        value["baseContent"] = json!(self.base_content);
        value["baseReasoning"] = json!(self.base_reasoning);
        value["baseKnown"] = json!(self.base_known);
        value
    }
}

/// One shadow of the formal reply receives validated pages; no event history or duplicate prefix is retained.
/// 只在正式回复的影子副本上接收已核对页面；不累计事件历史，也不另存整段旧前缀。
pub(super) struct StreamRecovery {
    content: String,
    reasoning: String,
    segments: Vec<SegmentBuffer>,
    indexes: HashMap<String,usize>,
    offsets: HashMap<String,(usize,usize)>,
    global_offsets: (usize,usize),
    after: u64,
    through: u64,
    task_sequence: u64,
    incomplete: Vec<&'static str>,
    segmented: bool,
}

fn take_text(value: &mut Value, field: &str) -> String {
    match value.get_mut(field).map(Value::take) {Some(Value::String(text))=>text,_=>String::new()}
}

/// Compare overlap byte-for-byte and append only the genuinely missing UTF-8 suffix.
/// 重叠部分逐字节核对，只追加真正缺失的UTF8后缀；水位以前缺字或任意冲突都不能猜测恢复。
fn recover_delta(text: &mut String, offset: &mut usize, delta: &str, append: bool) -> bool {
    if *offset>text.len() {return false;}
    let overlap=delta.len().min(text.len()-*offset);
    if text.as_bytes().get(*offset..*offset+overlap)!=delta.as_bytes().get(..overlap) {return false;}
    if overlap<delta.len() {
        if !append {return false;}
        let Some(suffix)=delta.get(overlap..) else {return false;};
        text.push_str(suffix);
    }
    *offset=offset.saturating_add(delta.len());
    true
}

impl StreamRecovery {
    pub(super) fn new(mut baseline:Value, after:u64) -> Self {
        let content=take_text(&mut baseline,"content");let reasoning=take_text(&mut baseline,"reasoning");
        let mut segments=Vec::new();let mut indexes=HashMap::new();let mut incomplete=Vec::new();
        for mut value in baseline["segments"].as_array_mut().map(std::mem::take).unwrap_or_default() {
            let Some(id)=value["id"].as_str().map(str::to_owned) else {incomplete.push("saved-segment-has-no-identity");continue;};
            if indexes.contains_key(&id) {incomplete.push("saved-segment-identity-collision");continue;}
            let segment=SegmentBuffer {content:take_text(&mut value,"content"),reasoning:take_text(&mut value,"reasoning"),
                metadata:value.as_object().cloned().unwrap_or_default(),..Default::default()};
            indexes.insert(id,segments.len());segments.push(segment);
        }
        let segmented=!segments.is_empty();
        if segmented {
            let (mut text_offset,mut thought_offset)=(0usize,0usize);
            let mut matches=true;
            for segment in &segments {
                if !segment.content.is_empty() {
                    if text_offset>0 {matches&=content.get(text_offset..text_offset+2)==Some("\n\n");text_offset+=2;}
                    matches&=content.get(text_offset..text_offset+segment.content.len())==Some(segment.content.as_str());
                    text_offset+=segment.content.len();
                }
                matches&=reasoning.get(thought_offset..thought_offset+segment.reasoning.len())==Some(segment.reasoning.as_str());
                thought_offset+=segment.reasoning.len();
            }
            if !matches || text_offset!=content.len() || thought_offset!=reasoning.len() {incomplete.push("saved-segment-prefix-mismatch");}
        }
        Self {content,reasoning,segments,indexes,offsets:HashMap::new(),global_offsets:(0,0),after,through:after,
            task_sequence:0,incomplete,segmented}
    }

    fn fail(&mut self, reason:&'static str) {if !self.incomplete.contains(&reason) {self.incomplete.push(reason);}}

    pub(super) fn observe(&mut self,event:&TaskEvent) {
        if !self.incomplete.is_empty() {return;}
        let payload=&event.payload;
        let Some(sequence)=payload["eventSequence"].as_u64() else {self.fail("legacy-event-has-no-stream-identity");return;};
        if sequence>self.after {
            if sequence<=self.through {self.fail("non-monotonic-stream-observation");return;}
            self.through=sequence;
        }
        self.task_sequence=event.sequence;
        let append=sequence>self.after;
        match event.kind.as_str() {
            "assistant_segment" => {
                let Some(id)=payload["segment"]["id"].as_str().or_else(||payload["segmentId"].as_str()) else {
                    self.fail("segment-has-no-stable-identity");return;
                };
                if payload["segment"]["emptyAtEmission"]==true {self.offsets.insert(id.to_owned(),(0,0));}
                if !append {return;}
                let index=if let Some(index)=self.indexes.get(id) {*index} else {
                    let index=self.segments.len();self.indexes.insert(id.to_owned(),index);
                    let mut metadata=serde_json::Map::new();metadata.insert("id".into(),json!(id));
                    self.segments.push(SegmentBuffer {metadata,..Default::default()});index
                };
                let segment=&mut self.segments[index];
                if let Some(metadata)=payload["segment"].as_object() {
                    for (key,value) in metadata {if !matches!(key.as_str(),"content"|"reasoning"|"emptyAtEmission") {
                        segment.metadata.insert(key.clone(),value.clone());
                    }}
                }
            }
            "text_delta"|"reasoning_delta" => {
                let Some(delta)=payload["delta"].as_str() else {self.fail("delta-requires-original");return;};
                let thinking=event.kind=="reasoning_delta";
                if let Some(id)=payload["segmentId"].as_str() {
                    if !self.segmented && self.segments.is_empty() && (!self.content.is_empty() || !self.reasoning.is_empty()) {
                        self.fail("saved-text-has-no-segment-baseline");return;
                    }
                    let index=if let Some(index)=self.indexes.get(id) {*index} else if append {
                        let index=self.segments.len();self.indexes.insert(id.to_owned(),index);
                        let mut metadata=serde_json::Map::new();metadata.insert("id".into(),json!(id));
                        self.segments.push(SegmentBuffer {metadata,..Default::default()});index
                    } else {self.fail("saved-segment-prefix-mismatch");return;};
                    let position=self.offsets.entry(id.to_owned()).or_default();
                    let segment=&mut self.segments[index];
                    let valid=if thinking {recover_delta(&mut segment.reasoning,&mut position.1,delta,append)}
                        else {recover_delta(&mut segment.content,&mut position.0,delta,append)};
                    if !valid {self.fail("snapshot-prefix-mismatch");}
                } else if self.segmented || !self.segments.is_empty() {self.fail("delta-has-no-segment-identity");}
                else {
                    let valid=if thinking {recover_delta(&mut self.reasoning,&mut self.global_offsets.1,delta,append)}
                        else {recover_delta(&mut self.content,&mut self.global_offsets.0,delta,append)};
                    if !valid {self.fail("snapshot-prefix-mismatch");}
                }
            }
            "content_snapshot" => {
                let Some(content)=payload["content"].as_str() else {self.fail("snapshot-requires-formal-original");return;};
                let Some(reasoning)=payload["reasoning"].as_str() else {self.fail("snapshot-requires-formal-original");return;};
                if !self.segments.is_empty() {self.fail("snapshot-requires-segment-originals");return;}
                if append {
                    self.content.clear();self.content.push_str(content);self.reasoning.clear();self.reasoning.push_str(reasoning);
                } else if !self.content.starts_with(content) || !self.reasoning.starts_with(reasoning) {
                    self.fail("snapshot-prefix-mismatch");return;
                }
                self.global_offsets=(content.len(),reasoning.len());
            }
            _=>{}
        }
    }

    pub(super) fn complete(&self)->bool {self.incomplete.is_empty()}

    pub(super) fn finish(mut self)->Value {
        if !self.incomplete.is_empty() {return json!({"complete":false,"incompleteReasons":self.incomplete});}
        self.segments.sort_by_key(|segment|segment.metadata.get("order").and_then(Value::as_u64).unwrap_or(u64::MAX));
        if !self.segments.is_empty() {
            for segment in &self.segments {
                if !segment.metadata.get("round").is_some_and(Value::is_u64)
                    || !segment.metadata.get("order").is_some_and(Value::is_u64)
                    || !matches!(segment.metadata.get("phase").and_then(Value::as_str),Some("answer"|"commentary"|"final_answer"))
                    || !matches!(segment.metadata.get("status").and_then(Value::as_str),Some("streaming"|"completed")) {
                    return json!({"complete":false,"incompleteReasons":["segment-requires-formal-original"]});
                }
            }
            self.content.clear();self.reasoning.clear();
            for segment in &self.segments {
                if !segment.content.is_empty() {if !self.content.is_empty() {self.content.push_str("\n\n");}self.content.push_str(&segment.content);}
                self.reasoning.push_str(&segment.reasoning);
            }
        }
        let segments=self.segments.into_iter().map(|segment| {
            let mut value=Value::Object(segment.metadata);value["content"]=json!(segment.content);value["reasoning"]=json!(segment.reasoning);value
        }).collect::<Vec<_>>();
        json!({"complete":true,"content":self.content,"reasoning":self.reasoning,"segments":segments,
            "throughStreamSequence":self.through,"throughTaskSequence":self.task_sequence,
            "evidence":"committed-display-only; no execution or completion proof"})
    }
}

pub(super) struct StreamProjection<'a> {
    conversation: &'a str,
    request: &'a str,
    reply: &'a str,
    after: u64,
    stream_sequence: u64,
    task_sequence: u64,
    replacement: bool,
    content: String,
    reasoning: String,
    base_content: String,
    base_reasoning: String,
    base_segments: HashMap<String, SegmentBuffer>,
    segment_base_known: bool,
    segments: Vec<SegmentBuffer>,
    segment_indexes: HashMap<String, usize>,
    incomplete: Vec<&'static str>,
    bytes: usize,
}

impl<'a> StreamProjection<'a> {
    pub(super) fn new(conversation: &'a str, request: &'a str, reply: &'a str, after: u64) -> Self {
        Self { conversation, request, reply, after, stream_sequence:after, task_sequence:0,
            replacement:false, content:String::new(), reasoning:String::new(), base_content:String::new(),
            base_reasoning:String::new(), base_segments:HashMap::new(), segment_base_known:true,
            segments:Vec::new(), segment_indexes:HashMap::new(), incomplete:Vec::new(), bytes:0 }
    }

    fn incomplete(&mut self, reason: &'static str) {
        if !self.incomplete.contains(&reason) { self.incomplete.push(reason); }
    }

    fn segment(&mut self, id: &str) -> &mut SegmentBuffer {
        let index = if let Some(index) = self.segment_indexes.get(id) { *index } else {
            let base = self.base_segments.get(id);
            let mut segment = SegmentBuffer { base_content:base.map(|base|base.content.clone()).unwrap_or_default(),
                base_reasoning:base.map(|base|base.reasoning.clone()).unwrap_or_default(),
                base_known:base.map(|base|base.base_known).unwrap_or(self.segment_base_known),..Default::default() };
            segment.metadata.insert("id".into(),json!(id));
            let index = self.segments.len();
            self.segments.push(segment);
            self.segment_indexes.insert(id.to_owned(),index);
            index
        };
        &mut self.segments[index]
    }

    pub(super) fn observe(&mut self, event: &TaskEvent) {
        let payload = &event.payload;
        let is_body = matches!(event.kind.as_str(), "text_delta" | "reasoning_delta" | "content_snapshot" | "assistant_segment");
        let Some(sequence) = payload["eventSequence"].as_u64() else {
            if is_body && event.sequence > self.after { self.incomplete("legacy-event-has-no-stream-identity"); }
            return;
        };
        if payload["conversationId"] != self.conversation || payload["requestId"] != self.request
            || payload["replyMessageId"] != self.reply { return; }
        if is_body {
            self.bytes = self.bytes.saturating_add(payload.to_string().len());
            if self.bytes > MAXIMUM_PROJECTION_BYTES { self.incomplete("projection-requires-paged-originals"); return; }
        }
        if sequence <= self.after {
            if event.kind == "content_snapshot" {
                if let (Some(content), Some(reasoning)) = (payload["content"].as_str(), payload["reasoning"].as_str()) {
                    self.base_content = content.to_owned(); self.base_reasoning = reasoning.to_owned();
                    self.base_segments.clear(); self.segment_base_known = false;
                }
            } else if event.kind == "assistant_segment" && payload["segment"]["emptyAtEmission"] == true {
                if let Some(id) = payload["segment"]["id"].as_str() {
                    self.base_segments.insert(id.to_owned(),SegmentBuffer {base_known:true,..Default::default()});
                }
            } else if matches!(event.kind.as_str(), "text_delta" | "reasoning_delta") {
                if let Some(delta) = payload["delta"].as_str() {
                    if event.kind == "text_delta" { self.base_content.push_str(delta); }
                        else { self.base_reasoning.push_str(delta); }
                    if let Some(id) = payload["segmentId"].as_str() {
                        let known = self.segment_base_known;
                        let segment = self.base_segments.entry(id.to_owned()).or_insert_with(||SegmentBuffer {base_known:known,..Default::default()});
                        if event.kind == "text_delta" {segment.content.push_str(delta);} else {segment.reasoning.push_str(delta);}
                    }
                }
            }
            return;
        }
        // A replay watermark can jump across private ledger events, but cannot repeat or reorder an SSE observation.
        // 回放水位可跳过私有账本事件，但同一请求回复的SSE观察不得重号或逆序。
        if sequence <= self.stream_sequence { self.incomplete("non-monotonic-stream-observation"); return; }
        self.stream_sequence = sequence;
        self.task_sequence = event.sequence;
        if !is_body { return; }
        match event.kind.as_str() {
            "content_snapshot" => {
                if let (Some(content), Some(reasoning)) = (payload["content"].as_str(), payload["reasoning"].as_str()) {
                    self.content = content.to_owned(); self.reasoning = reasoning.to_owned();
                    self.segments.clear(); self.replacement = true;
                    self.segment_indexes.clear();
                    self.base_segments.clear(); self.segment_base_known = false;
                } else { self.incomplete("snapshot-requires-formal-original"); }
            }
            "assistant_segment" => {
                if let Some(id) = payload["segment"]["id"].as_str().or_else(|| payload["segmentId"].as_str()) {
                    let segment = self.segment(id);
                    if payload["segment"]["emptyAtEmission"] == true {
                        segment.base_known=true; segment.base_content.clear(); segment.base_reasoning.clear();
                    }
                    if let Some(metadata) = payload["segment"].as_object() {
                        for (key, value) in metadata {
                            if !matches!(key.as_str(), "content" | "reasoning") { segment.metadata.insert(key.clone(),value.clone()); }
                        }
                    } else {
                        for key in ["phase", "status"] {
                            if !payload[key].is_null() { segment.metadata.insert(key.into(),payload[key].clone()); }
                        }
                    }
                } else { self.incomplete("segment-has-no-stable-identity"); }
            }
            "text_delta" | "reasoning_delta" => {
                let Some(delta) = payload["delta"].as_str() else { self.incomplete("delta-requires-original"); return; };
                if event.kind == "text_delta" { self.content.push_str(delta); } else { self.reasoning.push_str(delta); }
                if let Some(id) = payload["segmentId"].as_str() {
                    let segment = self.segment(id);
                    if event.kind == "text_delta" {segment.content.push_str(delta);} else {segment.reasoning.push_str(delta);}
                }
            }
            _ => {}
        }
    }

    pub(super) fn finish(self) -> Value {
        json!({"complete":self.incomplete.is_empty(),"incompleteReasons":self.incomplete,
            "requestId":self.request,"replyMessageId":self.reply,"afterStreamSequence":self.after,
            "throughStreamSequence":self.stream_sequence,"throughTaskSequence":self.task_sequence,
            "mode":if self.replacement {"replace"} else {"append"},"content":self.content,"reasoning":self.reasoning,
            "baseContent":self.base_content,"baseReasoning":self.base_reasoning,
            "segments":self.segments.into_iter().map(SegmentBuffer::project).collect::<Vec<_>>(),
            "evidence":"committed-display-only; no execution or completion proof"})
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn event(sequence: u64, kind: &str, content: Value) -> TaskEvent {
        let mut payload = json!({"type":kind,"conversationId":"chat","requestId":"request","replyMessageId":"reply",
            "eventSequence":sequence,"segmentId":"segment"});
        payload.as_object_mut().unwrap().extend(content.as_object().unwrap().clone());
        TaskEvent { task_id:"task".into(), sequence:sequence+5, event_id:format!("event-{sequence}"), kind:kind.into(), payload, created_at_ms:0 }
    }

    #[test]
    fn stream_projection_uses_only_committed_owned_suffix_and_preserves_segment_order() {
        let mut projection = StreamProjection::new("chat", "request", "reply", 2);
        projection.observe(&event(1,"text_delta",json!({"delta":"already saved"})));
        projection.observe(&event(3,"text_delta",json!({"delta":" new 🏰"})));
        let mut foreign = event(4,"text_delta",json!({"delta":"FOREIGN"}));
        foreign.payload["replyMessageId"] = json!("other"); projection.observe(&foreign);
        projection.observe(&event(5,"reasoning_delta",json!({"delta":"visible thought"})));
        projection.observe(&event(6,"assistant_segment",json!({"segment":{"id":"segment","order":1,"phase":"commentary","status":"completed"}})));
        let result = projection.finish();
        assert_eq!(result["complete"], true); assert_eq!(result["content"], " new 🏰");
        assert_eq!(result["segments"][0]["contentAppend"], " new 🏰");
        assert_eq!(result["segments"][0]["reasoningAppend"], "visible thought");
        assert_eq!(result["throughStreamSequence"], 6); assert!(!result.to_string().contains("FOREIGN"));
    }

    #[test]
    fn stream_projection_reports_missing_or_nonmonotonic_observations_without_forging_recovery() {
        let mut projection = StreamProjection::new("chat", "request", "reply", 0);
        projection.observe(&event(1,"content_snapshot",json!({"content":"base","reasoning":""})));
        projection.observe(&event(2,"text_delta",json!({"delta":" tail"})));
        let complete = projection.finish();
        assert_eq!(complete["mode"], "replace"); assert_eq!(complete["content"], "base tail");
        let mut projection = StreamProjection::new("chat", "request", "reply", 0);
        projection.observe(&event(2,"text_delta",json!({"delta":"saved"})));
        projection.observe(&event(2,"text_delta",json!({"delta":"duplicate"})));
        let result = projection.finish(); assert_eq!(result["complete"], false); assert_eq!(result["content"], "saved");
        let mut projection = StreamProjection::new("chat", "request", "reply", 0);
        projection.observe(&event(1,"content_snapshot",json!({"archived":true})));
        assert_eq!(projection.finish()["complete"], false);
    }

    #[test]
    fn stream_projection_many_small_unicode_deltas_preserve_segment_and_saved_prefix() {
        let mut projection=StreamProjection::new("chat","request","reply",1024);
        for sequence in 1..=2048 {
            projection.observe(&event(sequence,"text_delta",json!({"delta":"甲🏰"})));
        }
        projection.observe(&event(2049,"assistant_segment",json!({"segment":{
            "id":"segment","round":1,"order":1,"phase":"commentary","status":"completed"}})));
        let result=projection.finish();
        let expected="甲🏰".repeat(1024);
        assert_eq!(result["baseContent"],expected);
        assert_eq!(result["content"],expected);
        assert_eq!(result["segments"][0]["baseContent"],expected);
        assert_eq!(result["segments"][0]["contentAppend"],expected);
        assert_eq!(result["segments"][0]["status"],"completed");
        assert_eq!(result["throughStreamSequence"],2049);
    }

    #[test]
    fn stream_recovery_checks_overlap_and_keeps_conflicting_formal_baselines_unchanged() {
        let baseline=json!({"content":"first\n\nsecond par","reasoning":"","segments":[
            {"id":"one","round":1,"order":1,"phase":"commentary","status":"completed","content":"first","reasoning":""},
            {"id":"segment","round":2,"order":2,"phase":"commentary","status":"streaming","content":"second par","reasoning":""}]});
        let original=baseline.clone();
        let mut recovery=StreamRecovery::new(baseline.clone(),2);
        let mut first=event(1,"text_delta",json!({"delta":"first"}));first.payload["segmentId"]=json!("one");
        recovery.observe(&first);
        recovery.observe(&event(2,"text_delta",json!({"delta":"second "})));
        recovery.observe(&event(3,"text_delta",json!({"delta":"paragraph 🏰"})));
        let result=recovery.finish();
        assert_eq!(result["complete"],true);assert_eq!(result["content"],"first\n\nsecond paragraph 🏰");
        assert_eq!(result["throughStreamSequence"],3);assert_eq!(baseline,original);
        let mut bad=baseline.clone();bad["segments"][1]["content"]=json!("second wrong");bad["content"]=json!("first\n\nsecond wrong");
        let mut recovery=StreamRecovery::new(bad.clone(),2);
        recovery.observe(&first);recovery.observe(&event(2,"text_delta",json!({"delta":"second "})));
        recovery.observe(&event(3,"text_delta",json!({"delta":"paragraph 🏰"})));
        let result=recovery.finish();assert_eq!(result["complete"],false);
        assert!(result.get("content").is_none());assert!(result.get("throughStreamSequence").is_none());
        assert_eq!(bad["content"],"first\n\nsecond wrong");
    }
}

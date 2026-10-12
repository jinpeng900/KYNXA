//! Pure task continuation state and incremental public-text projection.
//! 纯任务续接状态与公开正文增量投影；执行、预算和持久化仍由编排调用端负责。

use std::collections::HashSet;
use std::sync::OnceLock;

use regex::Regex;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

/// Project public streaming text without retaining or rescanning the accumulated response.
/// 增量投影公开正文，不缓存或重新扫描累计回复；仅暂存内部标记的有限前缀。
pub struct TaskTextProjection {
    line_start: bool,
    fence: Option<(char, usize)>,
    fence_run: Option<(char, usize)>,
    fence_run_ended: bool,
    fence_tail_blank: bool,
    // Hold only a partial reserved opener; ordinary text is emitted immediately.
    // 只暂存未完整匹配的保留起始标记；普通正文立即输出。
    pending: String,
    hidden: bool,
    close_prefix: usize,
    closed: bool,
    removed_control: bool,
}

impl Default for TaskTextProjection {
    fn default() -> Self {
        Self {
            line_start: true,
            fence: None,
            fence_run: None,
            fence_run_ended: false,
            fence_tail_blank: true,
            pending: String::new(),
            hidden: false,
            close_prefix: 0,
            closed: false,
            removed_control: false,
        }
    }
}

impl TaskTextProjection {
    const OPEN: &'static str = "<kynxa_task>";
    const CLOSE: &'static [u8] = b"</kynxa_task>";

    pub fn push(&mut self, text: &str) -> String {
        let mut public = String::with_capacity(text.len());
        for character in text.chars() {
            let newline = matches!(character, '\n' | '\r' | '\u{2028}' | '\u{2029}');
            if self.hidden {
                // A complete reserved marker starts private control data, even if its JSON is invalid.
                // 完整保留标记之后属于私有控制数据；JSON 非法也不能泄露或升级任务状态。
                if !self.closed {
                    if character as u32 == Self::CLOSE[self.close_prefix] as u32 {
                        self.close_prefix += 1;
                        if self.close_prefix == Self::CLOSE.len() {
                            self.closed = true;
                        }
                    } else {
                        self.close_prefix = usize::from(character == '<');
                    }
                }
                if newline && self.closed {
                    self.hidden = false;
                    self.closed = false;
                    self.close_prefix = 0;
                    self.end_line();
                }
                continue;
            }
            if newline {
                public.push_str(&std::mem::take(&mut self.pending));
                public.push(character);
                self.end_line();
                continue;
            }
            if !self.pending.is_empty() {
                if Self::OPEN.as_bytes()[self.pending.len()] as u32 == character as u32 {
                    self.pending.push(character);
                    if self.pending.len() == Self::OPEN.len() {
                        self.pending.clear();
                        self.hidden = true;
                        self.removed_control = true;
                        self.line_start = false;
                    }
                } else {
                    public.push_str(&std::mem::take(&mut self.pending));
                    public.push(character);
                    self.line_start = false;
                }
                continue;
            }
            if self.line_start && matches!(character, ' ' | '\t') {
                public.push(character);
                continue;
            }
            if self.line_start && self.fence.is_none() && character == '<' {
                self.pending.push(character);
                continue;
            }
            if self.line_start && matches!(character, '`' | '~') {
                self.fence_run = Some((character, 1));
            } else if let Some((marker, count)) = &mut self.fence_run {
                if !self.fence_run_ended && character == *marker {
                    *count += 1;
                } else {
                    self.fence_run_ended = true;
                    self.fence_tail_blank &= matches!(character, ' ' | '\t');
                }
            }
            self.line_start = false;
            public.push(character);
        }
        public
    }

    pub fn finish(&mut self) -> String {
        // An unfinished marker prefix is ordinary text; completed control data remains private.
        // 未完成的标记前缀仍是普通正文；完整控制标记之后的内容保持私有。
        std::mem::take(&mut self.pending)
    }

    fn end_line(&mut self) {
        // Fence state is carried across chunks so literal examples retain their original text.
        // 围栏状态跨分块延续，保留示例中的标记原文。
        if let Some((marker, count)) = self.fence_run {
            if count >= 3 {
                match self.fence {
                    None => self.fence = Some((marker, count)),
                    Some((open, length))
                        if marker == open && count >= length && self.fence_tail_blank =>
                    {
                        self.fence = None;
                    }
                    _ => {}
                }
            }
        }
        self.line_start = true;
        self.fence_run = None;
        self.fence_run_ended = false;
        self.fence_tail_blank = true;
    }

    pub(super) fn project(text: &str) -> String {
        let mut projection = Self::default();
        let mut public = projection.push(text);
        public.push_str(&projection.finish());
        if projection.removed_control {
            public.truncate(public.trim_end_matches(javascript_whitespace).len());
        }
        public
    }
}

/// Semantic state is a model assessment, never an authorization or correctness certificate.
/// 语义状态来自模型判断，不能增加授权，也不能证明任务或事实正确。
#[derive(Default)]
pub struct TaskState {
    fingerprints: HashSet<String>,
    no_progress_rounds: u64,
    resumes: u64,
    completion_reviews: u64,
    review_requests: u64,
    last_state: Option<Value>,
    last_review: Option<Value>,
}

impl TaskState {
    pub fn apply(&mut self, operation: &str, args: &Value) -> Result<Value, String> {
        match operation {
            "task.inspect" => Ok(self.inspect(string_field(args, "text"))),
            "task.observe" => {
                let normalized: String = string_field(args, "content")
                    .chars()
                    .filter(|character| !javascript_whitespace(*character))
                    .collect();
                let fingerprint = format!("{:x}", Sha256::digest(normalized.as_bytes()));
                let useful = !normalized.is_empty() && !self.fingerprints.contains(&fingerprint);
                self.no_progress_rounds = if useful {
                    0
                } else {
                    self.no_progress_rounds + 1
                };
                if !normalized.is_empty() {
                    self.fingerprints.insert(fingerprint);
                }
                Ok(json!(useful))
            }
            "task.resumed" => {
                self.resumes += 1;
                Ok(json!(self.resumes))
            }
            "task.reviewStarted" => {
                self.completion_reviews += 1;
                Ok(json!(self.completion_reviews))
            }
            "task.reviewRequested" => {
                self.review_requests += 1;
                Ok(json!(self.review_requests))
            }
            "task.reviewResult" => {
                self.last_review = args.get("value").filter(|value| !value.is_null()).cloned();
                Ok(self.last_review.clone().unwrap_or(Value::Null))
            }
            "task.audit" => Ok(self.audit(args.get("status").unwrap_or(&Value::Null))),
            "task.state" => Ok(self.snapshot()),
            "task.decide" => Ok(self.decide(args)),
            _ => Err("UNKNOWN_AGENT_CORE_OPERATION".into()),
        }
    }

    fn inspect(&mut self, content: &str) -> Value {
        // Only a standalone final footer outside a code fence can control a subsequent turn.
        // 只有代码围栏之外的独立末行尾部才能影响后续轮次；非法控制数据同样不公开。
        let projected = TaskTextProjection::project(content);
        static FOOTER: OnceLock<Regex> = OnceLock::new();
        let footer = FOOTER.get_or_init(|| {
            Regex::new(r"(?:^|\n)[ \t]*<kynxa_task>([^\n]{1,1024})</kynxa_task>[ \t\r\n]*$")
                .expect("static task footer expression")
        });
        if let Some(captures) = footer.captures(content) {
            let matched = captures.get(0).expect("complete regex match");
            let prefix = &content[..matched.start()];
            if !in_fence(prefix) {
                if let Ok(value) = serde_json::from_str::<Value>(&captures[1]) {
                    if valid_footer(&value) {
                        let public_content = projected.trim_end_matches(javascript_whitespace);
                        let mut status = json!({
                            "content": public_content,
                            "state": string_field(&value, "state"),
                            "next": string_field(&value, "next"),
                            "reason": string_field(&value, "reason"),
                            "source": "model-task-state",
                        });
                        if status["state"] == "complete" && handoff(public_content) {
                            status["state"] = json!("review");
                            status["source"] = json!("conflicting-completion-cue");
                        }
                        // Snapshots are control metadata; the response body is returned only by inspect.
                        // 快照只保留控制元数据；正文仅由 inspect 返回，避免每次 IPC 重传长回复。
                        let mut metadata = status.clone();
                        metadata
                            .as_object_mut()
                            .expect("task status object")
                            .remove("content");
                        self.last_state = Some(metadata);
                        return status;
                    }
                }
            }
        }
        let needs_review = handoff(content);
        json!({
            "content": projected,
            "state": if needs_review { "review" } else { "unknown" },
            "next": "",
            "reason": "",
            "source": if needs_review { "unresolved-handoff-cue" } else { "legacy-response" },
        })
    }

    fn audit(&self, status: &Value) -> Value {
        let mut audit = json!({
            "state": string_field(status, "state"),
            "source": string_field(status, "source"),
            "next": string_field(status, "next"),
            "reason": string_field(status, "reason"),
            "resumes": self.resumes,
            "noProgressRounds": self.no_progress_rounds,
            "taskCorrectnessCertified": false,
            "completionReviews": self.completion_reviews,
            "reviewRequests": self.review_requests,
        });
        if let Some(review) = &self.last_review {
            audit["completionReview"] = review.clone();
        }
        audit
    }

    fn snapshot(&self) -> Value {
        json!({
            "resumes": self.resumes,
            "noProgressRounds": self.no_progress_rounds,
            "completionReviews": self.completion_reviews,
            "reviewRequests": self.review_requests,
            "lastState": self.last_state,
            "lastReview": self.last_review,
        })
    }

    fn decide(&self, args: &Value) -> Value {
        // Return a decision only; the caller owns archiving, resource admission, and the next model request.
        // 这里只返回决策；归档、资源准入和下一次模型请求由调用端执行。
        let reply = args.get("reply").unwrap_or(&Value::Null);
        let state = string_field(reply, "state");
        let has_calls = boolean_field(args, "hasCalls")
            || reply
                .get("calls")
                .and_then(Value::as_array)
                .is_some_and(|calls| !calls.is_empty());
        let truncated = boolean_field(args, "outputTruncated");
        let exhausted = args.get("remainingRounds").and_then(Value::as_u64) == Some(0);

        if boolean_field(args, "isReview") {
            let valid = !has_calls
                && !truncated
                && string_field(reply, "source") == "model-task-state"
                && matches!(state, "complete" | "continue" | "blocked")
                && (state != "complete"
                    || !string_field(reply, "reason")
                        .trim_matches(javascript_whitespace)
                        .is_empty());
            if !valid {
                return if exhausted
                    || args
                        .get("reviewAttempts")
                        .and_then(Value::as_u64)
                        .unwrap_or(0)
                        >= 2
                {
                    decision("interrupt", Some("TASK_COMPLETION_REVIEW_INVALID"))
                } else {
                    decision("retry-review", Some("TASK_COMPLETION_REVIEW_REPAIR"))
                };
            }
            if state == "blocked" {
                return json!({ "action": "interrupt", "code": "TASK_BLOCKED", "reviewed": true });
            }
            if state == "continue" {
                if self.no_progress_rounds >= 2 {
                    return json!({
                        "action": "interrupt",
                        "code": "TASK_CONTINUATION_NO_PROGRESS",
                        "reviewed": true
                    });
                }
                return json!({ "action": "resume", "reviewed": true, "exhaustedBudget": exhausted });
            }
            return json!({ "action": "final", "reviewed": true });
        }

        let can_continue = !boolean_field(args, "summarizeOnly")
            && !boolean_field(args, "recoveryFinal")
            && !boolean_field(args, "hasUnknownEffects");
        let continue_task = !has_calls && can_continue && matches!(state, "continue" | "review");
        if state == "blocked" && !has_calls {
            return decision("interrupt", Some("TASK_BLOCKED"));
        }
        if self.resumes > 0 && self.no_progress_rounds >= 2 && continue_task {
            return decision("interrupt", Some("TASK_CONTINUATION_NO_PROGRESS"));
        }
        if truncated || continue_task {
            if has_calls {
                return decision("interrupt", Some("OUTPUT_CONTINUATION_INVALID"));
            }
            // Archive the current output before enforcing the last-round boundary.
            // 当前正文必须先归档；不能因最后一轮预算耗尽而丢掉已经生成的内容。
            return json!({
                "action": "resume",
                "taskContinuation": continue_task,
                "exhaustedBudget": exhausted
            });
        }
        if !has_calls
            && can_continue
            && args
                .get("outputContinuations")
                .and_then(Value::as_u64)
                .unwrap_or(0)
                > 0
            && matches!(state, "unknown" | "complete")
        {
            return json!({ "action": "review", "exhaustedBudget": exhausted });
        }
        decision("final", None)
    }
}

fn decision(action: &str, code: Option<&str>) -> Value {
    match code {
        Some(code) => json!({ "action": action, "code": code }),
        None => json!({ "action": action }),
    }
}

fn string_field<'a>(value: &'a Value, key: &str) -> &'a str {
    value.get(key).and_then(Value::as_str).unwrap_or("")
}

fn boolean_field(value: &Value, key: &str) -> bool {
    value.get(key).and_then(Value::as_bool).unwrap_or(false)
}

fn valid_footer(value: &Value) -> bool {
    let Some(object) = value.as_object() else {
        return false;
    };
    let state = string_field(value, "state");
    matches!(state, "continue" | "complete" | "blocked")
        && object
            .keys()
            .all(|key| matches!(key.as_str(), "state" | "next" | "reason"))
        && ["next", "reason"].iter().all(|key| {
            object.get(*key).is_none_or(|item| {
                item.as_str()
                    .is_some_and(|text| text.encode_utf16().count() <= 512)
            })
        })
        && (state != "continue"
            || !string_field(value, "next")
                .trim_matches(javascript_whitespace)
                .is_empty())
        && (state != "blocked"
            || !string_field(value, "reason")
                .trim_matches(javascript_whitespace)
                .is_empty())
}

fn in_fence(content: &str) -> bool {
    content
        .split(['\n', '\r', '\u{2028}', '\u{2029}'])
        .filter(|line| {
            let line = line.trim_start_matches([' ', '\t']);
            line.starts_with("```") || line.starts_with("~~~")
        })
        .count()
        % 2
        != 0
}

fn handoff(content: &str) -> bool {
    static PARAGRAPH_BREAK: OnceLock<Regex> = OnceLock::new();
    static HANDOFF: OnceLock<Regex> = OnceLock::new();
    let content = content.trim_matches(javascript_whitespace);
    let paragraph_break = PARAGRAPH_BREAK.get_or_init(|| {
        Regex::new(r"\n[\u{0009}-\u{000D}\u{0020}\u{00A0}\u{1680}\u{2000}-\u{200A}\u{2028}\u{2029}\u{202F}\u{205F}\u{3000}\u{FEFF}]*\n")
            .expect("static paragraph expression")
    });
    let tail = paragraph_break.split(content).last().unwrap_or("");
    let prefix = &content[..content.len() - tail.len()];
    let expression = HANDOFF.get_or_init(|| Regex::new(
        r"^(?:下一(?:段|章|步)|接下来|随后)[^\n。]{0,48}(?:将继续|会继续|我将继续|继续(?:从|写|完成|处理|执行))[^\n]{0,180}[。.!]?$"
    ).expect("static handoff expression"));
    !in_fence(prefix)
        && expression.is_match(tail)
        && !['“', '”', '"', '「', '」']
            .iter()
            .any(|character| tail.contains(*character))
        && !["建议", "你可以", "您可以", "如果", "例如"]
            .iter()
            .any(|word| tail.contains(word))
}

// JavaScript's whitespace set excludes U+0085 and includes the BOM; keep cross-runtime fingerprints stable.
// JavaScript 空白集合不含 U+0085、包含 BOM；保持跨运行时指纹与裁剪一致。
fn javascript_whitespace(character: char) -> bool {
    matches!(
        character,
        '\u{0009}'..='\u{000D}'
            | '\u{0020}'
            | '\u{00A0}'
            | '\u{1680}'
            | '\u{2000}'..='\u{200A}'
            | '\u{2028}'
            | '\u{2029}'
            | '\u{202F}'
            | '\u{205F}'
            | '\u{3000}'
            | '\u{FEFF}'
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn invalid_control_footers_are_private_without_certifying_completion() {
        for content in [
            "正文\n<kynxa_task>{broken}</kynxa_task>",
            "正文\n<kynxa_task>{\"state\":\"continue\",\"next\":\"go\",\"resumes\":3}</kynxa_task>",
        ] {
            let result = TaskState::default().inspect(content);
            assert_eq!(result["state"], "unknown");
            assert_eq!(result["content"], "正文");
        }
        for content in [
            "```text\n<kynxa_task>{\"state\":\"continue\",\"next\":\"go\"}</kynxa_task>",
            "> <kynxa_task>{\"state\":\"complete\"}</kynxa_task>",
        ] {
            let result = TaskState::default().inspect(content);
            assert_eq!(result["state"], "unknown");
            assert_eq!(result["content"], content);
        }
    }

    #[test]
    fn streaming_projection_hides_control_across_every_character_boundary() {
        let text = "你好😀。\n  <kynxa_task>{\"state\":\"complete\"}</kynxa_task>\n";
        for boundary in text
            .char_indices()
            .map(|(index, _)| index)
            .chain([text.len()])
        {
            let mut projection = TaskTextProjection::default();
            let public = [
                projection.push(&text[..boundary]),
                projection.push(&text[boundary..]),
                projection.finish(),
            ]
            .concat();
            assert_eq!(public, "你好😀。\n  ");
            assert!(projection.pending.len() < TaskTextProjection::OPEN.len());
        }
        let mut projection = TaskTextProjection::default();
        let mut chunks: Vec<String> = text
            .chars()
            .map(|character| projection.push(&character.to_string()))
            .collect();
        chunks.push(projection.finish());
        assert_eq!(chunks.concat(), "你好😀。\n  ");
        let mut projection = TaskTextProjection::default();
        assert_eq!(projection.push("普通文本\n<kynxa_t"), "普通文本\n");
        assert_eq!(projection.finish(), "<kynxa_t");
        let mut projection = TaskTextProjection::default();
        assert_eq!(
            projection.push("普通文本\n<kynxa_task>{broken"),
            "普通文本\n"
        );
        assert_eq!(projection.finish(), "");
    }

    #[test]
    fn streaming_projection_preserves_literal_quoted_inline_and_fenced_examples() {
        for text in [
            "介绍 <kynxa_task>{broken}</kynxa_task> 作为例子。",
            "> <kynxa_task>{broken}</kynxa_task>\n后续正文",
            "`<kynxa_task>{broken}</kynxa_task>`",
            "```json\n<kynxa_task>{broken}</kynxa_task>\n```\n正文",
            "~~~~text\n<kynxa_task>{broken}</kynxa_task>\n~~~\n<kynxa_task>仍在围栏中</kynxa_task>\n~~~~\n正文",
        ] {
            let mut projection = TaskTextProjection::default();
            let mut public: Vec<String> = text
                .chars()
                .map(|character| projection.push(&character.to_string()))
                .collect();
            public.push(projection.finish());
            assert_eq!(public.concat(), text);
        }
        assert_eq!(
            TaskTextProjection::project(
                "```json\n例子\n```\n正文\n<kynxa_task>{broken}</kynxa_task>"
            ),
            "```json\n例子\n```\n正文"
        );
    }

    #[test]
    fn streaming_projection_emits_long_normal_lines_without_accumulating_them() {
        let text = "文".repeat(200_000);
        let mut projection = TaskTextProjection::default();
        assert_eq!(projection.push(&text), text);
        assert!(projection.pending.is_empty());
        assert_eq!(projection.push("还有即时正文"), "还有即时正文");
        assert_eq!(projection.finish(), "");
    }

    #[test]
    fn metadata_obeys_utf16_and_conflicting_completion_limits() {
        let mut state = TaskState::default();
        let make = |next: String| {
            format!(
                "正文\n<kynxa_task>{}</kynxa_task>",
                json!({"state":"continue", "next":next})
            )
        };
        assert_eq!(state.inspect(&make("😀".repeat(256)))["state"], "continue");
        assert_eq!(state.inspect(&make("😀".repeat(257)))["state"], "unknown");
        let result = state.inspect(
            "正文\n\n下一章将继续从原位置写起。\n<kynxa_task>{\"state\":\"complete\"}</kynxa_task>",
        );
        assert_eq!(result["state"], "review");
        assert_eq!(result["source"], "conflicting-completion-cue");
    }

    #[test]
    fn normalized_replay_is_no_progress_without_certification() {
        let mut state = TaskState::default();
        assert_eq!(
            state
                .apply("task.observe", &json!({"content":"甲 乙"}))
                .unwrap(),
            true
        );
        assert_eq!(
            state
                .apply("task.observe", &json!({"content":"甲\u{FEFF}乙"}))
                .unwrap(),
            false
        );
        assert_eq!(
            state
                .apply("task.observe", &json!({"content":"甲\n乙"}))
                .unwrap(),
            false
        );
        assert_eq!(
            state.audit(&json!({"state":"complete"}))["noProgressRounds"],
            2
        );
        assert_eq!(
            state.audit(&json!({"state":"complete"}))["taskCorrectnessCertified"],
            false
        );
    }

    #[test]
    fn state_snapshot_does_not_retransmit_the_response_body() {
        let mut state = TaskState::default();
        let content = format!(
            "{}\n<kynxa_task>{{\"state\":\"complete\"}}</kynxa_task>",
            "正文".repeat(200_000)
        );
        let inspected = state.inspect(&content);
        assert_eq!(
            inspected["content"].as_str().unwrap().chars().count(),
            400_000
        );
        let snapshot = state.snapshot();
        assert!(snapshot.to_string().len() < 1024);
        assert!(snapshot["lastState"].get("content").is_none());
    }

    #[test]
    fn final_round_archives_before_interrupt_and_review_never_executes_calls() {
        let state = TaskState::default();
        let continued = state.decide(&json!({"reply":{"state":"continue"},"remainingRounds":0}));
        assert_eq!(continued["action"], "resume");
        assert_eq!(continued["exhaustedBudget"], true);
        let review = state.decide(&json!({"reply":{"state":"complete","source":"model-task-state","reason":"done"},"isReview":true,"hasCalls":true,"reviewAttempts":2}));
        assert_eq!(review["code"], "TASK_COMPLETION_REVIEW_INVALID");
        assert_eq!(
            state.decide(&json!({"reply":{"state":"complete"},"outputContinuations":1}))["action"],
            "review"
        );
    }
}

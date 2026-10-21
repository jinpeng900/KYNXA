//! Pure task continuation state and incremental public-text projection.
//! 纯任务续接状态与公开正文增量投影；执行、预算和持久化仍由编排调用端负责。

use std::collections::HashSet;
use std::sync::OnceLock;

use regex::Regex;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

/// Successful completion requires a settled receipt, not a queued command or an unknown dispatch.
/// 完成证据须来自已结算的成功回执；排队、运行中、未知或失败的派发不能证明任务完成。
pub(crate) fn successful_completion_receipt(receipt: &Value) -> bool {
    if receipt["status"] != "completed" || receipt["isError"] == true || receipt["executed"] == false {
        return false;
    }
    let content = &receipt["content"];
    ![content, &content["command"], &content["structuredContent"]].into_iter().any(|result| {
        result["isError"] == true || result["success"] == false || result["completed"] == false
            || result.get("error").is_some_and(|error| !error.is_null() && error != false && error != "")
            || result["exitCode"].as_i64().is_some_and(|code| code != 0)
            || result["code"].as_str().is_some_and(|code| code.split('_').any(|part| part.eq_ignore_ascii_case("UNKNOWN")))
            || ["state", "status", "outcome"].into_iter().any(|field| matches!(result[field].as_str(),
                Some("running" | "queued" | "pending" | "unknown" | "failed" | "error" | "cancelled" | "interrupted")))
    })
}

/// Only a writer's committed-content receipt can certify an artifact path.
/// 只有写入工具真实提交内容的回执可证明输出路径；输入路径及普通读取不能冒充产物。
pub(crate) fn completed_output_paths(name: &str, receipt: &Value) -> Vec<Value> {
    if !successful_completion_receipt(receipt)
        || !matches!(name, "filesystem.write" | "filesystem.edit" | "document.create") {
        return Vec::new();
    }
    let content = &receipt["content"];
    if content["sha256"].as_str().is_none_or(|hash| hash.len() != 64 || !hash.bytes().all(|byte| byte.is_ascii_hexdigit()))
        || content["bytes"].as_u64().is_none() {
        return Vec::new();
    }
    content["path"].as_str().filter(|path| !path.is_empty()).map(|path| vec![json!(path)]).unwrap_or_default()
}

/// Discovery and orchestration maintenance establish no external outcome by themselves.
/// 发现目录与编排维护本身不能证明外部目标已经达成。
pub(crate) fn is_completion_evidence_tool(name: &str) -> bool {
    !matches!(name, "tool.search" | "tool.load" | "context.compact" | "task.describe" | "task.status" | "task.wait")
}

/// Explicit external actions and current-source questions require actual execution evidence; prose does not.
/// 明确外部动作与当下来源问题须有真实执行证据；问候、文案、解释和纯运算不额外触发工具验收。
pub(crate) fn requires_execution_verification(goal: &str) -> bool {
    static EXPLANATION: OnceLock<Regex> = OnceLock::new();
    static ACTION: OnceLock<Regex> = OnceLock::new();
    static NEGATED_ACTION: OnceLock<Regex> = OnceLock::new();
    static CLAUSES: OnceLock<Regex> = OnceLock::new();
    static EXPLANATORY_ACTION: OnceLock<Regex> = OnceLock::new();
    if super::budget::is_finite_response_request(goal) { return false; }
    let explanation=EXPLANATION.get_or_init(|| Regex::new(concat!(
        r"(?ix)^\s*(?:请|能否|可以|please\s+)?(?:解释|说明|教我|告诉我(?:如何|怎么)|介绍|explain\b|describe\b|teach\s+me\b|how\s+(?:do|can|to)\b)"
    )).expect("static explanatory task expression"));
    let additional_action=EXPLANATORY_ACTION.get_or_init(||Regex::new(concat!(
        r"(?ix)(?:并且?|同时|然后|and\s+(?:then\s+|also\s+)?)(?:请|帮我|为我|please\s+)?",
        r"(?:打开|启动|运行|执行|安装|下载|上传|搜索|抓取|读取|保存|导出|提交|修改|修复|创建|生成|制作|测试|删除|open\b|launch\b|run\b|execute\b|install\b|download\b|upload\b|search\b|read\b|save\b|export\b|modify\b|fix\b|create\b|build\b|test\b|delete\b)"
    )).expect("static explanatory additional action expression"));
    let negative=NEGATED_ACTION.get_or_init(||Regex::new(concat!(
        r"(?ix)^\s*(?:(?:请|先|这次|这轮|本轮|当前|please)\s*)*(?:(?:不要|不再|无需|不用|不需要|禁止|别)",
        r"(?:再|继续|去|额外|实际|自动|重新|主动|帮我|任何|进行)*\s*",
        r"(?:打开|启动|运行|执行|安装|卸载|下载|上传|抓取|爬取|搜索|检索|浏览|访问|回读|读取|读写|读|扫描|渲染|建模|写入|写|保存|导出|提交|推送|修改|修复|重构|编译|测试|检查|验证|调用|删除|删|创建|生成|制作|更新)",
        r"|(?:don't|do\s+not|never|without|no\s+need\s+to)\s+(?:actually\s+|continue\s+|further\s+|automatically\s+)*",
        r"(?:open|launch|run|execute|install|uninstall|download|upload|fetch|scrape|browse|render|inspect|compile|deploy|commit|push|read|write|save|export|edit|modify|delete|create|build|verify|test|search|call|use)\b)"
    )).expect("static negated execution task expression"));
    let clauses=CLAUSES.get_or_init(||Regex::new(r"(?i)[，,；;。！？!?\n\r]+|(?:但是|但请|而是|\bbut\b|\binstead\b)").expect("static request clause expression"));
    let positive=clauses.split(goal).filter(|clause|!negative.is_match(clause)
        && (!explanation.is_match(clause) || additional_action.is_match(clause))).collect::<Vec<_>>().join("\n");
    ACTION.get_or_init(|| Regex::new(concat!(
        r"(?ix)(?:打开|启动|运行|执行|安装|卸载|下载|上传|抓取|爬取|搜索网页|网页搜索|检索网页|浏览网页|访问网站|回读|读取|扫描|渲染|建模|写入|保存|导出|提交|推送|合并分支|修改代码|修复代码|重构代码|编译|运行测试|测试代码)",
        r"|(?:创建|生成|新建|编辑|修改|删除|更新|制作|检查|验证|核对).{0,40}(?:文件|文档|目录|文件夹|代码|工程|项目|场景|模型|数据库|网页|网站|图像|图片|图表|演示|表格|幻灯片|进程|应用|软件)",
        r"|(?:创建|生成|新建|编辑|修改|读取|保存|create\b|generate\b|make\b|write\b|edit\b|save\b|read\b).{0,50}(?:\.(?:txt|md|rs|py|js|ts|cs|docx|xlsx|pptx|pdf|blend|png|jpg)\b|\b(?:docx|xlsx|pptx|pdf|blend)\b)",
        r"|(?:最新|今天|当前|现在|实时).{0,30}(?:新闻|版本|价格|天气|比分|汇率|政策|公告|发布|信息)",
        r"|\b(?:open|launch|execute|install|uninstall|download|upload|fetch|scrape|browse|render|inspect|compile|deploy|commit|push)\b",
        r"|\b(?:run|read|scan|save|export|edit|modify|fix|delete|create|build|verify|test|search)\b.{0,60}\b(?:file|folder|directory|code|project|scene|application|app|software|web|website|browser|terminal|command|test|tests|blender|document|spreadsheet|presentation)\b",
        r"|\b(?:latest|current|today|live)\b.{0,40}\b(?:news|version|price|weather|score|rate|release|information)\b"
    )).expect("static external execution task expression")).is_match(&positive)
}

/// Quality claims need an observed result, not only a successful mutation or a saved path.
/// 质量要求须观察实际结果，不能只凭修改成功或保存路径完成验收。
pub(crate) fn requires_result_readback(goal: &str) -> bool {
    static QUALITY: OnceLock<Regex> = OnceLock::new();
    requires_execution_verification(goal) && QUALITY.get_or_init(|| Regex::new(concat!(
        r"(?ix)回读|检查|验证|核对|渲染|预览|画面|视觉|精致|细节|美观|质量|金碧|排版|测试",
        r"|\b(?:read\s+back|readback|inspect|verify|validate|render|preview|visual|beautiful|detailed|quality|test|tests)\b"
    )).expect("static observed quality task expression")).is_match(goal)
}

/// Visual acceptance requires image input to the evaluating model, not a text description of a render.
/// 视觉验收要求评估模型实际收到图像，不能拿渲染的文本描述或文件名代替。
pub(crate) fn requires_visual_observation(goal: &str) -> bool {
    static VISUAL: OnceLock<Regex> = OnceLock::new();
    requires_execution_verification(goal) && VISUAL.get_or_init(|| Regex::new(concat!(
        r"(?ix)渲染|建模|画面|视觉|精致|美观|金碧|配色|排版|幻灯片|演示文稿",
        r"|\b(?:blender|render|rendering|visual|beautiful|aesthetic|layout|slide|slides|presentation)\b"
    )).expect("static visual observation task expression")).is_match(goal)
}

/// Only actual embedded image bytes can be visual input; paths, media placeholders and prose do not qualify.
/// 只有真实嵌入图像字节才可能成为视觉输入；路径、媒体占位和描述文字不算。
pub(crate) fn receipt_has_visual_observation(canonical: &Value) -> bool {
    !receipt_visual_hashes(canonical).is_empty()
}

fn encoded_visual_hash(encoded: &str) -> Option<String> {
    use base64::Engine;
    let bytes=base64::engine::general_purpose::STANDARD.decode(encoded).ok()?;
    (!bytes.is_empty()).then(||format!("{:x}",Sha256::digest(&bytes)))
}

/// Compare decoded media bytes, not a call ID appearing alongside an unrelated older image.
/// 比较实际解码的媒体字节；不能因当前调用 ID 与旧图同时出现就认定新图已经投递。
pub(crate) fn receipt_visual_hashes(canonical:&Value)->Vec<String> {
    let mut hashes:Vec<String>=canonical["content"].as_array().into_iter().flatten().filter(|block|
        block["type"]=="image" && matches!(block["mimeType"].as_str(),Some("image/png"|"image/jpeg"|"image/webp"|"image/gif")))
        .filter_map(|block|block["data"].as_str().and_then(encoded_visual_hash)).collect();
    hashes.sort();hashes.dedup();hashes
}

/// Only typed image inputs in the actual provider body count; text mentions and media placeholders do not.
/// 只取实际供应商请求中带类型的图像输入；文本提及、路径与媒体占位不算模型已看到。
pub(crate) fn model_visual_hashes(body:&Value)->Vec<String> {
    fn visit(value:&Value,hashes:&mut Vec<String>) {
        if value["type"]=="image" && value["source"]["type"]=="base64" {
            if let Some(hash)=value["source"]["data"].as_str().and_then(encoded_visual_hash) { hashes.push(hash); }
        }else if matches!(value["type"].as_str(),Some("image_url"|"input_image")) {
            let url=value["image_url"].as_str().or_else(||value["image_url"]["url"].as_str());
            if let Some((header,data))=url.and_then(|url|url.split_once(";base64,")) {
                if header.starts_with("data:image/") {if let Some(hash)=encoded_visual_hash(data){hashes.push(hash);}}
            }
        }
        match value {
            Value::Object(object)=>for child in object.values(){visit(child,hashes);},
            Value::Array(items)=>for child in items{visit(child,hashes);},
            _=>{},
        }
    }
    let mut hashes=Vec::new();visit(body,&mut hashes);
    // Ollama's native wire places raw base64 under each actual message's images array.
    // Ollama 原生协议把纯 base64 放在实际消息的 images 数组中，普通文本仍不参与判断。
    for message in body["messages"].as_array().into_iter().flatten() {
        for encoded in message["images"].as_array().into_iter().flatten().filter_map(Value::as_str) {
            if let Some(hash)=encoded_visual_hash(encoded){hashes.push(hash);}
        }
    }
    hashes.sort();hashes.dedup();hashes
}

/// Successful readback tools return observable content or state; discovery and mutation receipts are excluded.
/// 成功的回读工具返回可观察内容或状态，发现目录和修改回执不能冒充回读。
pub(crate) fn receipt_is_readback(tool: &str, canonical: &Value) -> bool {
    if receipt_has_visual_observation(canonical) { return true; }
    if tool.starts_with("terminal.") && (canonical["exitCode"].is_i64() || canonical["command"]["exitCode"].is_i64()) {
        return true;
    }
    matches!(tool, "filesystem.read" | "document.read" | "web.fetch" | "web.search" | "browser.read"
        | "browser.snapshot" | "browser.screenshot" | "knowledge.read" | "knowledge.search"
        | "tool.result.read" | "terminal.session.read" | "terminal.host.read" | "process.list")
        || tool.rsplit('.').next().is_some_and(|name| matches!(name,
            "get_scene_info" | "get_object_info" | "list_scene_items" | "get_world_state_snapshot"))
}

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
            Regex::new(r"(?:^|\n)[ \t]*<kynxa_task>([^\n]+)</kynxa_task>[ \t\r\n]*$")
                .expect("static task footer expression")
        });
        if let Some(captures) = footer.captures(content) {
            let matched = captures.get(0).expect("complete regex match");
            let prefix = &content[..matched.start()];
            // The length limit is checked here: a large repetition bound exceeds the regex size limit.
            // 长度上限在此检查：正则中写大重复次数会超出编译大小限制。
            if !in_fence(prefix) && captures[1].chars().count() <= 16384 {
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
                        // Optional structured fields let completion and blocking be checked without phrase matching.
                        // 可选结构字段用于核对完成与受阻，不依赖措辞匹配。
                        if let Some(unmet) = value.get("unmet") {
                            status["unmet"] = unmet.clone();
                        }
                        if let Some(blocker) = value.get("blocker") {
                            status["blocker"] = blocker.clone();
                        }
                        if let Some(met) = value.get("met") {
                            status["met"] = met.clone();
                        }
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

        let mut remaining = remaining_work(reply);
        if boolean_field(args, "isReview") {
            // With real effects, a completion is accepted only when every claimed item cites real evidence.
            // 有真实操作时，只有每项已满足的要求都引用了真实证据，完成才被接受。
            if (boolean_field(args, "evidenceRequired") || boolean_field(args, "requiresVerification"))
                && state == "complete" && remaining.is_empty() {
                let valid = args.get("validEvidence").and_then(Value::as_array).cloned().unwrap_or_default();
                let expected = args.get("expectedRequirements").and_then(Value::as_array).cloned().unwrap_or_default();
                remaining = unproven_requirements(reply, &valid, &expected);
                if boolean_field(args, "requiresReadback") && !boolean_field(args, "hasReadback") {
                    remaining.push("Observe and read back the actual result before certifying its requested quality; a saved path or mutation receipt is not sufficient.".into());
                }
                if boolean_field(args, "requiresVisualReadback") && !boolean_field(args, "hasVisualReadback") {
                    remaining.push("Render or capture the actual output and deliver its image to the evaluating model before visual acceptance; text or an image filename is not visual evidence.".into());
                }
            }
            let valid = !has_calls
                && !truncated
                && string_field(reply, "source") == "model-task-state"
                && matches!(state, "complete" | "continue" | "blocked");
            if !valid {
                // A malformed assessment is repairable control metadata, never a reason to end useful work.
                // 非法验收结构属于可修正控制元数据，不能据固定两次次数结束仍可推进的工作。
                return json!({"action":"retry-review","code":"TASK_COMPLETION_REVIEW_REPAIR",
                    "exhaustedBudget":exhausted,"repairMode":if args["reviewAttempts"].as_u64().unwrap_or(0) >= 2 {
                        "rebuild-from-checkpoint" } else { "repair-footer" }});
            }
            // A self-reported completion that still lists unmet requirements or a next step is not complete.
            // 自报完成但仍列出未达要求或下一步，不能当作完成；difficulty 也不是外部受阻。
            let reviewed_continue = state == "continue"
                || state == "complete" && !remaining.is_empty()
                || state == "blocked" && !genuine_blocker(reply);
            if state == "blocked" && !reviewed_continue {
                return json!({ "action": "interrupt", "code": "TASK_BLOCKED", "reviewed": true });
            }
            if reviewed_continue {
                if remaining.is_empty() && state == "blocked" {
                    remaining.push("Change the implementation strategy and choose a concrete authorized next step using the saved evidence.".into());
                }
                return json!({ "action": "resume", "reviewed": true, "exhaustedBudget": exhausted,
                    "remaining": remaining,"replan":self.no_progress_rounds >= 2 });
            }
            if below_minimum_active(args) {
                return json!({ "action": "resume", "reviewed": true, "exhaustedBudget": exhausted,
                    "minimumActive": true });
            }
            return json!({ "action": "final", "reviewed": true });
        }

        let can_continue = !boolean_field(args, "summarizeOnly")
            && !boolean_field(args, "recoveryFinal")
            && !boolean_field(args, "hasUnknownEffects");
        // Difficulty with an executable next step stays inside the authorized task.
        // 有可执行下一步的困难仍属于原授权任务，不能当作外部受阻收尾。
        let difficulty = state == "blocked" && !genuine_blocker(reply);
        let continue_task = !has_calls
            && can_continue
            && (matches!(state, "continue" | "review") || difficulty || state == "complete" && !remaining.is_empty());
        if state == "blocked" && !has_calls && !continue_task {
            return decision("interrupt", Some("TASK_BLOCKED"));
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
                "exhaustedBudget": exhausted,
                "replan":self.no_progress_rounds >= 2
            });
        }
        if !has_calls
            && can_continue
            && (args["supportsTaskMarkers"] != false || boolean_field(args, "requiresVerification"))
            // A reply to live guidance can acknowledge work without completing the existing task.
            // 实时插话后的正文可能只是确认调整；原任务需沿用完成核对，不能仅因没有工具调用就结束。
            // Real external effects also need a review: unknown or self-reported completion is not proof.
            // 已执行真实外部操作的任务同样需要核对：unknown 或自报完成都不能证明原任务已完成。
            && (args
                .get("outputContinuations")
                .and_then(Value::as_u64)
                .unwrap_or(0)
                > 0
                || boolean_field(args, "steeredTask")
                || boolean_field(args, "steeredToolTask")
                || boolean_field(args, "effectfulWork")
                || boolean_field(args, "requiresVerification"))
            && matches!(state, "unknown" | "complete")
        {
            return json!({ "action": "review", "exhaustedBudget": exhausted });
        }
        decision("final", None)
    }
}

/// Structured remaining work from a footer: explicit unmet requirements first, then the stated next step.
/// 从尾部结构取得剩余工作：优先使用明确的未达要求，再使用模型给出的下一步。
pub(crate) fn remaining_work(reply: &Value) -> Vec<String> {
    let mut items: Vec<String> = reply
        .get("unmet")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .map(|item| item.trim_matches(javascript_whitespace).to_owned())
        .filter(|item| !item.is_empty())
        .collect();
    let next = string_field(reply, "next").trim_matches(javascript_whitespace);
    if items.is_empty() && !next.is_empty() {
        items.push(next.to_owned());
    }
    items
}

/// Only a declared external condition ends a task as blocked; plain difficulty does not.
/// 只有声明的外部条件才能以受阻结束任务；单纯的实现困难不算。
pub(crate) fn genuine_blocker(reply: &Value) -> bool {
    matches!(
        string_field(reply, "blocker"),
        "user-input" | "authorization" | "external" | "resource"
    )
}

/// A useful work budget survives segment exhaustion; the caller checkpoints and renews its next segment.
/// 有意义的工作预算跨执行段保留；执行段耗尽后由调用端保存检查点并续接，不当作已完成。
fn below_minimum_active(args: &Value) -> bool {
    let minimum = args.get("minimumActiveMs").and_then(Value::as_u64).unwrap_or(0);
    let active = args.get("activeElapsedMs").and_then(Value::as_u64).unwrap_or(u64::MAX);
    minimum > 0 && active < minimum
}

/// Preserve explicit request clauses as acceptance; stable requirement IDs avoid copying long prose in a footer.
/// 将用户明确的请求分句保留为验收项；稳定要求 ID 避免尾部控制结构重复长篇正文。
pub(crate) fn acceptance_requirements(message: &str) -> Vec<String> {
    let mut requirements = Vec::new();
    for clause in message.split(['\n', '\r', ';', '；', '。']) {
        let clause = clause.trim();
        if !clause.is_empty() && !requirements.iter().any(|existing| existing == clause) {
            if requirements.len() < 128 {
                requirements.push(clause.to_owned());
            } else if let Some(last) = requirements.last_mut() {
                // The repository supports 128 entries: fold excess clauses without silently dropping them.
                // 仓储支持 128 条；超出的分句合并到末项，不静默丢弃要求。
                last.push('\n');
                last.push_str(clause);
            }
        }
    }
    if requirements.is_empty() {
        requirements.push("根据用户附件进行回答。 / Answer using the attached content.".into());
    }
    requirements
}

/// Only explicit continuation language carries an unfinished task's goal and work-time remainder.
/// 只有明确续作措辞才携带未完成任务的目标和剩余工作预算，新话题不自动继承。
pub(crate) fn is_explicit_continuation(message: &str) -> bool {
    static CONTINUATION: OnceLock<Regex> = OnceLock::new();
    CONTINUATION.get_or_init(|| Regex::new(concat!(
        r"(?ix)^\s*(?:请|麻烦|帮我|那就|好|好的|ok|please|okay|ok\s*)?\s*",
        r"(?:(?:继续|接着|接下来|续上)(?:\s*[吧啊呀，,。.!！]?\s*$|\s*(?:刚才|上次|之前|原来|当前|这个|上述|剩余|未完成|精修|完善|推进|执行|做|工作|优化|修改|建模|修复|测试|验证|完成))",
        r"|(?:continue|resume|carry\s+on)(?:\s*[.!]?\s*$|\s+(?:the\s+)?(?:previous|unfinished|remaining|same|current|work|task|refin|build|implement|test|fix)))"
    )).expect("static explicit continuation expression")).is_match(message)
}

pub(crate) fn has_continuation_details(message: &str) -> bool {
    static BARE: OnceLock<Regex> = OnceLock::new();
    !BARE.get_or_init(|| Regex::new(
        r"(?ix)^\s*(?:请|麻烦|帮我|那就|好|好的|ok|please|okay)?\s*(?:继续|接着|续上|continue|resume|carry\s+on)\s*(?:吧|啊|呀|please)?\s*[，,。.!！]?\s*$"
    ).expect("static bare continuation expression")).is_match(message)
}

/// Detect an echoed internal history record; it is never an answer or proof of a current action.
/// 识别模型复述的内部历史调用记录；它既不是答复，也不能证明本轮执行了操作。
pub(crate) fn contains_history_record(text: &str) -> bool {
    static RECORD: OnceLock<Regex> = OnceLock::new();
    RECORD
        .get_or_init(|| Regex::new(r#"\{\s*"historicalToolCalls"\s*:"#).expect("static history record expression"))
        .is_match(text)
}

/// Remove echoed history-record JSON objects from public text, keeping surrounding prose.
/// 从公开正文移除复述的历史记录 JSON 对象，保留前后的普通文字。
pub(crate) fn strip_history_records(text: &str) -> String {
    let mut filter = HistoryEchoFilter::default();
    let mut output = filter.push(text);
    output.push_str(&filter.finish());
    output.trim_matches(javascript_whitespace).to_owned()
}

/// Streaming filter that withholds echoed history-record objects before any delta reaches the user.
/// 流式过滤器：在增量送达用户之前扣留复述的历史记录对象；空格、换行、代码块和分块边界都不影响识别。
#[derive(Default)]
pub struct HistoryEchoFilter {
    pending: String,
    hiding: bool,
}

impl HistoryEchoFilter {
    const KEY: &'static str = "\"historicalToolCalls\"";
    /// An unterminated record larger than this is dropped rather than buffered without bound.
    /// 超过该大小仍未闭合的记录直接丢弃，不无限缓存。
    const MAXIMUM_RECORD_BYTES: usize = 4 * 1024 * 1024;

    pub fn push(&mut self, text: &str) -> String {
        self.pending.push_str(text);
        let mut public = String::new();
        loop {
            if self.hiding {
                let mut stream = serde_json::Deserializer::from_str(&self.pending).into_iter::<Value>();
                match stream.next() {
                    Some(Ok(_)) => {
                        let end = stream.byte_offset();
                        self.pending.drain(..end);
                        self.hiding = false;
                        continue;
                    }
                    Some(Err(error)) if error.is_eof() => {
                        if self.pending.len() > Self::MAXIMUM_RECORD_BYTES {
                            self.pending.clear();
                        }
                        return public;
                    }
                    // A malformed record is still internal data; drop it instead of publishing it.
                    // 格式损坏的记录仍属内部数据，丢弃而不是公开。
                    _ => {
                        self.pending.clear();
                        self.hiding = false;
                        return public;
                    }
                }
            }
            let Some(start) = self.pending.find('{') else {
                public.push_str(&std::mem::take(&mut self.pending));
                return public;
            };
            public.push_str(&self.pending[..start]);
            self.pending.drain(..start);
            let after = self.pending[1..].trim_start();
            if after.starts_with(Self::KEY) {
                self.hiding = true;
                continue;
            }
            if Self::KEY.starts_with(after) {
                // Still ambiguous; wait for the next chunk before publishing the brace.
                // 仍无法判断；等下一块到达再决定是否公开这个花括号。
                return public;
            }
            public.push('{');
            self.pending.drain(..1);
        }
    }

    pub fn finish(&mut self) -> String {
        if self.hiding {
            self.pending.clear();
            self.hiding = false;
            return String::new();
        }
        std::mem::take(&mut self.pending)
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
            .all(|key| matches!(key.as_str(), "state" | "next" | "reason" | "unmet" | "blocker" | "met"))
        && object.get("met").is_none_or(valid_met)
        && object.get("unmet").is_none_or(|unmet| {
            unmet.as_array().is_some_and(|items| {
                items.len() <= 128
                    && items.iter().all(|item| {
                        item.as_str()
                            .is_some_and(|text| text.encode_utf16().count() <= 256)
                    })
            })
        })
        && object.get("blocker").is_none_or(|blocker| {
            matches!(
                blocker.as_str(),
                Some("user-input" | "authorization" | "external" | "resource" | "difficulty")
            )
        })
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

/// Each claimed requirement names its evidence: tool call IDs or artifact paths from this task.
/// 每项已满足的要求都要列出证据：本任务真实执行的调用 ID 或产物路径。
fn valid_met(met: &Value) -> bool {
    met.as_array().is_some_and(|items| {
        items.len() <= 128
            && items.iter().all(|item| {
                item.as_object().is_some_and(|object| {
                    object.keys().all(|key| matches!(key.as_str(), "item" | "evidence" | "requirementId"))
                        && object.get("requirementId").is_none_or(|id| id.as_str().is_some_and(|id| {
                            id.strip_prefix("requirement-").and_then(|number| number.parse::<usize>().ok()).is_some_and(|number| number > 0)
                        }))
                        && item["item"].as_str().is_some_and(|text| text.encode_utf16().count() <= 256)
                        && item["evidence"].as_array().is_some_and(|evidence| {
                            evidence.len() <= 8
                                && evidence.iter().all(|entry| {
                                    entry.as_str().is_some_and(|text| text.encode_utf16().count() <= 512)
                                })
                        })
                })
            })
    })
}

/// Requirements claimed as met whose evidence matches no real call ID or artifact path of this task.
/// 自称已满足、但证据对不上本任务真实调用 ID 或产物路径的要求；它们按未完成处理。
fn unproven_requirements(reply: &Value, valid_evidence: &[Value], expected: &[Value]) -> Vec<String> {
    let normalize = |text: &str| text.trim().replace('\\', "/").to_lowercase();
    let known = |entry: &str| {
        let entry = entry.trim();
        let windows_path = |path: &str| path.as_bytes().get(1) == Some(&b':') || path.starts_with("\\\\") || path.starts_with("//");
        if entry.is_empty() { return false; }
        // Call IDs are exact, opaque identities; only Windows filesystem paths are case-insensitive.
        // 调用 ID 是必须精确匹配的不透明身份；仅 Windows 文件路径按不区分大小写处理。
        if valid_evidence.iter().filter_map(Value::as_str).any(|valid| valid.trim() == entry
            || windows_path(valid) && normalize(valid) == normalize(entry)) {
            return true;
        }
        if entry.contains('/') || entry.contains('\\') { return false; }
        // Preserve old basename references only when they uniquely identify an evidenced artifact.
        // 旧格式的文件名引用仅在能唯一定位已证明产物时兼容，不能匹配任意调用 ID。
        valid_evidence.iter().filter_map(Value::as_str).filter(|valid| {
            let valid_path = valid.trim().replace('\\', "/");
            if windows_path(valid) {
                valid_path.to_lowercase().ends_with(&format!("/{}", entry.to_lowercase()))
            } else {
                valid_path.ends_with(&format!("/{entry}"))
            }
        }).count() == 1
    };
    let Some(items) = reply.get("met").and_then(Value::as_array).filter(|items| !items.is_empty()) else {
        return vec!["List each explicit requirement in met with its tool call IDs or artifact paths as evidence.".into()];
    };
    let mut remaining: Vec<String> = items
        .iter()
        .filter(|item| {
            !item["evidence"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(Value::as_str)
                .any(known)
        })
        .map(|item| format!("Provide real evidence for: {}", item["item"].as_str().unwrap_or("")))
        .collect();
    for (index, requirement) in expected.iter().filter_map(Value::as_str).enumerate() {
        let id = format!("requirement-{}", index + 1);
        let covered = items.iter().any(|item| {
            (item["requirementId"].as_str() == Some(id.as_str())
                || normalize(item["item"].as_str().unwrap_or("")) == normalize(requirement))
                && item["evidence"].as_array().into_iter().flatten().filter_map(Value::as_str).any(known)
        });
        if !covered {
            remaining.push(format!("Verify {id} with successful execution evidence: {requirement}"));
        }
    }
    remaining
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
    fn final_round_archives_before_continuation_and_invalid_reviews_are_repaired() {
        let state = TaskState::default();
        let continued = state.decide(&json!({"reply":{"state":"continue"},"remainingRounds":0}));
        assert_eq!(continued["action"], "resume");
        assert_eq!(continued["exhaustedBudget"], true);
        let review = state.decide(&json!({"reply":{"state":"complete","source":"model-task-state","reason":"done"},"isReview":true,"hasCalls":true,"reviewAttempts":2}));
        assert_eq!(review["action"], "retry-review");
        assert_eq!(review["code"], "TASK_COMPLETION_REVIEW_REPAIR");
        assert_eq!(review["repairMode"], "rebuild-from-checkpoint");
        assert_eq!(
            state.decide(&json!({"reply":{"state":"complete"},"outputContinuations":1}))["action"],
            "review"
        );
    }

    #[test]
    fn effectful_work_reviews_unknown_or_self_reported_completion_but_plain_chat_finishes() {
        let state = TaskState::default();
        let unknown = json!({"state":"unknown","source":"legacy-response"});
        assert_eq!(state.decide(&json!({"reply":unknown,"effectfulWork":true}))["action"], "review");
        assert_eq!(state.decide(&json!({"reply":{"state":"complete","source":"model-task-state"},
            "effectfulWork":true}))["action"], "review");
        // Ordinary chat and tool-free answers keep their single request.
        assert_eq!(state.decide(&json!({"reply":unknown}))["action"], "final");
        assert_eq!(state.decide(&json!({"reply":unknown,"effectfulWork":true,
            "supportsTaskMarkers":false}))["action"], "final");
        assert_eq!(state.decide(&json!({"reply":unknown,"requiresVerification":true}))["action"],"review");
        assert_eq!(state.decide(&json!({"reply":{"state":"complete"},"requiresVerification":true,
            "supportsTaskMarkers":false}))["action"],"review");
    }

    #[test]
    fn execution_and_visual_acceptance_distinguish_plain_answers_from_observed_external_work() {
        for goal in ["你好","说你好100个小时","2 + 2 等于多少","写一段生日祝福","解释如何打开 Blender", "what model are you"] {
            assert!(!requires_execution_verification(goal),"{goal}");
        }
        for goal in ["打开 Blender","创建报告.docx","Create created.txt and read the result.","Inspect rendered Blender image quality.","查一下今天的天气信息"] {
            assert!(requires_execution_verification(goal),"{goal}");
        }
        assert!(requires_visual_observation("用 Blender 渲染金色穹顶并检查画面"));
        let state = TaskState::default();
        let complete = json!({"state":"complete","source":"model-task-state","met":[{"item":"view","requirementId":"requirement-1","evidence":["write"]}]});
        let missing_image = state.decide(&json!({"reply":complete,"isReview":true,"requiresVerification":true,
            "expectedRequirements":["检查实际画面"],"validEvidence":["write"],"requiresReadback":true,"hasReadback":true,
            "requiresVisualReadback":true,"hasVisualReadback":false}));
        assert_eq!(missing_image["action"],"resume");
        assert!(missing_image["remaining"].as_array().unwrap().iter().any(|item|item.as_str().unwrap().contains("image")));
        let mut semantic = TaskState::default();
        for attempt in 0..5 {
            let repair = semantic.decide(&json!({"reply":{"state":"unknown"},"isReview":true,"reviewAttempts":attempt,"remainingRounds":0}));
            assert_eq!(repair["action"],"retry-review");
            let _ = semantic.apply("task.observe",&json!({"content":"same failed approach"}));
        }
        let difficulty = semantic.decide(&json!({"reply":{"state":"blocked","source":"model-task-state","reason":"The first method failed"},"isReview":true}));
        assert_eq!(difficulty["action"],"resume");
        assert_eq!(difficulty["replan"],true);
    }

    #[test]
    fn negated_operations_and_attachment_only_answers_do_not_force_external_execution() {
        for goal in ["更新方案：保留原范围，只解释结果，不再继续写入。",
            "Don't delete the source code, only explain the result without running tools.",
            "Explain how to open Blender.\nExplain how to open Blender.",
            "根据用户附件进行回答。 / Answer using the attached content."] {
            assert!(!requires_execution_verification(goal),"{goal}");
        }
        for goal in ["不要删源码，修改项目代码。","不能工作，请修复代码。",
            "Do not delete the code, but fix the project code.","解释报错并修复代码"] {
            assert!(requires_execution_verification(goal),"{goal}");
        }
        assert!(!requires_execution_verification(&acceptance_requirements("").join("\n")));
    }

    #[test]
    fn visual_delivery_hashes_match_actual_image_inputs_instead_of_call_id_mentions() {
        let current=json!({"content":[{"type":"image","mimeType":"image/png","data":"bmV3LWltYWdl"}]});
        let current_hash=receipt_visual_hashes(&current);
        assert_eq!(current_hash.len(),1);
        let stale=json!({"messages":[{"role":"user","content":[{"type":"text","text":"current-tool-call-id"},
            {"type":"image_url","image_url":{"url":"data:image/png;base64,b2xkLWltYWdl"}}]}]});
        assert!(!model_visual_hashes(&stale).iter().any(|hash|current_hash.contains(hash)));
        let visible=json!({"input":[{"role":"user","content":[{"type":"input_image","image_url":"data:image/png;base64,bmV3LWltYWdl"}]}]});
        assert_eq!(model_visual_hashes(&visible),current_hash);
        let anthropic=json!({"messages":[{"role":"user","content":[{"type":"image","source":{"type":"base64","media_type":"image/png","data":"bmV3LWltYWdl"}}]}]});
        assert_eq!(model_visual_hashes(&anthropic),current_hash);
        let ollama=json!({"messages":[{"role":"user","content":"Inspect this image","images":["bmV3LWltYWdl"]}]});
        assert_eq!(model_visual_hashes(&ollama),current_hash);
        assert!(model_visual_hashes(&json!({"messages":[{"role":"user","content":"data:image/png;base64,bmV3LWltYWdl"}]})).is_empty());
    }

    #[test]
    fn review_completion_with_unmet_requirements_or_difficulty_keeps_working() {
        let mut state = TaskState::default();
        let unmet = state.inspect("已渲染。\n<kynxa_task>{\"state\":\"complete\",\"next\":\"\",\"reason\":\"\",\"unmet\":[\"彩窗\",\"金箔穹顶\"]}</kynxa_task>");
        assert_eq!(unmet["source"], "model-task-state");
        let decision = state.decide(&json!({"reply":unmet,"isReview":true}));
        assert_eq!(decision["action"], "resume");
        assert_eq!(decision["remaining"], json!(["彩窗", "金箔穹顶"]));
        let difficulty = state.inspect("雕像需重做。\n<kynxa_task>{\"state\":\"blocked\",\"next\":\"改用布尔凹龛与独立雕塑资产\",\"reason\":\"四次修正仍不可辨\"}</kynxa_task>");
        assert_eq!(state.decide(&json!({"reply":difficulty,"isReview":true}))["action"], "resume");
        assert_eq!(state.decide(&json!({"reply":difficulty}))["action"], "resume");
        let external = state.inspect("需要账号。\n<kynxa_task>{\"state\":\"blocked\",\"next\":\"用户登录后继续\",\"reason\":\"需要登录\",\"blocker\":\"user-input\"}</kynxa_task>");
        assert_eq!(state.decide(&json!({"reply":external,"isReview":true}))["code"], "TASK_BLOCKED");
        assert_eq!(state.decide(&json!({"reply":external}))["code"], "TASK_BLOCKED");
        let invalid = state.inspect("x\n<kynxa_task>{\"state\":\"blocked\",\"reason\":\"r\",\"blocker\":\"tired\"}</kynxa_task>");
        assert_eq!(invalid["source"], "legacy-response");
    }

    #[test]
    fn evidence_backed_completion_rejects_claims_without_real_calls_or_artifacts() {
        let mut state = TaskState::default();
        let valid = json!(["call_7", "C:\\work\\cathedral_v2.blend", "https://example.com/a.png"]);
        let proven = state.inspect(r#"完成。
<kynxa_task>{"state":"complete","next":"","reason":"","unmet":[],"met":[{"item":"保存工程","evidence":["cathedral_v2.blend"]},{"item":"渲染","evidence":["call_7"]}]}</kynxa_task>"#);
        assert_eq!(proven["source"], "model-task-state");
        assert_eq!(state.decide(&json!({"reply":proven,"isReview":true,"evidenceRequired":true,"validEvidence":valid}))["action"], "final");
        let invented = state.inspect(r#"完成。
<kynxa_task>{"state":"complete","next":"","reason":"","unmet":[],"met":[{"item":"彩窗","evidence":["window_render.png"]}]}</kynxa_task>"#);
        let decision = state.decide(&json!({"reply":invented,"isReview":true,"evidenceRequired":true,"validEvidence":valid}));
        assert_eq!(decision["action"], "resume");
        assert!(decision["remaining"][0].as_str().unwrap().contains("彩窗"));
        let bare = state.inspect("完成。\n<kynxa_task>{\"state\":\"complete\",\"next\":\"\",\"reason\":\"\"}</kynxa_task>");
        assert_eq!(state.decide(&json!({"reply":bare,"isReview":true,"evidenceRequired":true,"validEvidence":valid}))["action"], "resume");
        // Reviews that did not follow real effects keep the original acceptance rule.
        assert_eq!(state.decide(&json!({"reply":bare,"isReview":true}))["action"], "final");
    }

    #[test]
    fn minimum_active_duration_continues_a_reviewed_completion_across_segment_exhaustion() {
        let state = TaskState::default();
        let complete = json!({"state":"complete","source":"model-task-state"});
        let early = state.decide(&json!({"reply":complete,"isReview":true,
            "minimumActiveMs":3_600_000,"activeElapsedMs":600_000,"remainingRounds":10}));
        assert_eq!(early["action"], "resume");
        assert_eq!(early["minimumActive"], true);
        assert_eq!(state.decide(&json!({"reply":complete,"isReview":true,
            "minimumActiveMs":3_600_000,"activeElapsedMs":3_700_000,"remainingRounds":10}))["action"], "final");
        let exhausted = state.decide(&json!({"reply":complete,"isReview":true,
            "minimumActiveMs":3_600_000,"activeElapsedMs":600_000,"remainingRounds":0}));
        assert_eq!(exhausted["action"], "resume");
        assert_eq!(exhausted["exhaustedBudget"], true);
        assert_eq!(exhausted["minimumActive"], true);
    }

    #[test]
    fn completion_review_covers_every_authoritative_requirement_with_successful_evidence() {
        let mut state = TaskState::default();
        let complete = state.inspect(r#"完成。
<kynxa_task>{"state":"complete","met":[{"item":"保存工程","requirementId":"requirement-1","evidence":["call_success"]}],"unmet":[]}</kynxa_task>"#);
        let args = json!({"reply":complete,"isReview":true,"evidenceRequired":true,
            "validEvidence":["call_success"],"expectedRequirements":["保存可编辑工程","渲染并检查彩窗"]});
        let missing = state.decide(&args);
        assert_eq!(missing["action"], "resume");
        assert!(missing["remaining"][0].as_str().unwrap().contains("requirement-2"));
        let mut proven = args.clone();
        proven["reply"]["met"].as_array_mut().unwrap().push(json!({"item":"渲染检查","requirementId":"requirement-2","evidence":["call_success"]}));
        assert_eq!(state.decide(&proven)["action"], "final");
        proven["reply"]["met"][1]["evidence"] = json!(["call_failed"]);
        assert_eq!(state.decide(&proven)["action"], "resume");
        proven["reply"]["met"][1]["evidence"] = json!(["call_success"]);
        proven["reply"]["met"][1]["requirementId"] = json!("requirement-1");
        assert_eq!(state.decide(&proven)["action"], "resume");
        let mut wrong_identity = args.clone();
        wrong_identity["reply"]["met"][0]["evidence"] = json!(["CALL_SUCCESS"]);
        assert!(state.decide(&wrong_identity)["remaining"].as_array().unwrap().iter()
            .any(|item| item.as_str().unwrap().contains("Provide real evidence")));

        let expected: Vec<_> = (1..=17).map(|index| format!("验收要求 {index}")).collect();
        let met: Vec<_> = expected.iter().enumerate().map(|(index, item)| json!({
            "item":item,"requirementId":format!("requirement-{}",index + 1),"evidence":["call_success"]
        })).collect();
        let footer = format!("完成。\n<kynxa_task>{}</kynxa_task>", json!({"state":"complete","met":met}));
        let all = state.inspect(&footer);
        assert_eq!(all["source"], "model-task-state");
        assert_eq!(state.decide(&json!({"reply":all,"isReview":true,"evidenceRequired":true,
            "validEvidence":["call_success"],"expectedRequirements":expected}))["action"], "final");
    }

    #[test]
    fn explicit_requirements_and_continuation_keep_the_original_work_scope() {
        assert_eq!(acceptance_requirements("生成工程；不要座椅\n使用法线贴图。检查彩窗"),
            vec!["生成工程", "不要座椅", "使用法线贴图", "检查彩窗"]);
        for message in ["继续", "继续精修，把穹顶改成金色", "请继续之前未完成的任务", "resume the previous task"] {
            assert!(is_explicit_continuation(message), "{message}");
        }
        for message in ["你好", "北京天气怎么样", "另一个问题，解释 BCNF", "请创建新项目"] {
            assert!(!is_explicit_continuation(message), "{message}");
        }
        assert!(!has_continuation_details("继续吧"));
        assert!(has_continuation_details("继续精修，把穹顶改金色"));
    }

    #[test]
    fn echoed_history_records_are_detected_and_removed_from_public_text() {
        let echoed = "我继续做。\n\n{\"historicalToolCalls\":true,\"round\":2,\"text\":\"先落实三项\",\"calls\":[{\"id\":\"h_1\",\"arguments\":{\"code\":\"print('}')\"}}],\"notice\":\"x\"} 结尾";
        assert!(contains_history_record(echoed));
        assert_eq!(strip_history_records(echoed), "我继续做。\n\n 结尾");
        assert_eq!(strip_history_records("{\"historicalToolCalls\":true,\"calls\":["), "");
        assert!(!contains_history_record("普通回答"));
    }

    #[test]
    fn streaming_history_filter_withholds_records_across_every_chunk_boundary() {
        let echoed = "继续。\n```json\n{ \"historicalToolCalls\" : true,\n  \"calls\": [{\"arguments\":{\"code\":\"x = {'a': 1}\"}}]\n}\n```\n下一步 {a} 与 {\"b\":1}";
        let expected = "继续。\n```json\n\n```\n下一步 {a} 与 {\"b\":1}";
        for split in 0..=echoed.len() {
            if !echoed.is_char_boundary(split) {
                continue;
            }
            let mut filter = HistoryEchoFilter::default();
            let mut public = filter.push(&echoed[..split]);
            assert!(!public.contains("historicalToolCalls"), "{split}");
            public.push_str(&filter.push(&echoed[split..]));
            public.push_str(&filter.finish());
            assert_eq!(public, expected, "split at {split}");
        }
        assert!(contains_history_record(echoed));
        let mut unterminated = HistoryEchoFilter::default();
        assert_eq!(unterminated.push("前文 {\"historicalToolCalls\":true,\"calls\":["), "前文 ");
        assert_eq!(unterminated.finish(), "");
    }

    #[test]
    fn completion_review_accepts_the_same_empty_reason_as_a_valid_footer() {
        let mut state = TaskState::default();
        let reply = state.inspect(
            "成果已保存。\n<kynxa_task>{\"state\":\"complete\",\"next\":\"\",\"reason\":\"\"}</kynxa_task>",
        );
        assert_eq!(reply["source"], "model-task-state");
        assert_eq!(
            state.decide(&json!({"reply":reply,"isReview":true}))["action"],
            "final"
        );
        assert_eq!(
            state.decide(&json!({"reply":reply,"isReview":true,"outputTruncated":true}))["action"],
            "retry-review"
        );
    }

    #[test]
    fn plain_text_continuation_delivers_final_text_without_requesting_a_forbidden_marker() {
        let state = TaskState::default();
        let args = json!({
            "reply":{"state":"unknown","source":"legacy-response"},
            "outputContinuations":1,
            "supportsTaskMarkers":false,
        });
        assert_eq!(state.decide(&args)["action"], "final");
        let mut truncated = args;
        truncated["outputTruncated"] = json!(true);
        assert_eq!(state.decide(&truncated)["action"], "resume");
    }

    #[test]
    fn steered_text_work_reviews_unassessed_completion_without_changing_ordinary_chat() {
        let state = TaskState::default();
        let mut args = json!({
            "reply":{"state":"unknown","source":"legacy-response"},
            "outputContinuations":0,"supportsTaskMarkers":true,
        });
        assert_eq!(state.decide(&args)["action"], "final");
        args["steeredTask"] = json!(true);
        assert_eq!(state.decide(&args)["action"], "review");
        let mut legacy = args.clone();
        legacy.as_object_mut().unwrap().remove("steeredTask");
        legacy["steeredToolTask"] = json!(true);
        assert_eq!(state.decide(&legacy)["action"], "review");
        args["reply"] = json!({"state":"complete","source":"model-task-state"});
        assert_eq!(state.decide(&args)["action"], "review");
        for boundary in ["hasCalls", "hasUnknownEffects", "summarizeOnly", "recoveryFinal"] {
            let mut guarded = args.clone();
            guarded[boundary] = json!(true);
            assert_eq!(state.decide(&guarded)["action"], "final", "{boundary}");
        }
        args["reply"] = json!({"state":"blocked","source":"model-task-state"});
        assert_eq!(state.decide(&args)["action"], "resume");
        args["reply"]["blocker"] = json!("external");
        assert_eq!(state.decide(&args)["code"], "TASK_BLOCKED");
    }
}

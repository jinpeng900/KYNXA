use std::collections::{HashMap, HashSet};
use std::sync::{Mutex, OnceLock};

use regex::Regex;
use serde_json::{Value, json};

const LOCAL: &str = r"(?i)之前|记得|记忆|资料|文档|知识|项目|工作|代码|文件|我们讨论|上次|history|remember|document|knowledge|project|code|file|previous";
const GREETING: &str = r"(?i)^(?:你好|您好|嗨|哈喽|早上好|晚上好|早安|晚安|hello|hi|hey|good morning|good evening)[\s!！。.~～]*$";
const SMALL_TALK: &str = r"(?i)^(?:谢谢(?:你)?|多谢|好的|好吧|没事|嗯|聊聊天|陪我聊聊|讲个笑话|最近怎么样|今天心情怎么样|thanks?(?: you)?|okay|ok|how are you|tell me a joke)[\s!！?？。.~～]*$";
const KNOWLEDGE: &str = r"(?i)资料|文档|知识|记忆|记得|回忆|历史|我们讨论|上次|(?:之前|以前).{0,16}(?:说|讲|讨论|决定|确认|记)|\b(?:history|remember|recall|knowledge|documentation|docs?|previous\s+(?:discussion|decision|conversation)|saved\s+(?:memory|notes?))\b";
const ANALYSIS: &str = r"(?i)分析|解释|讲解|总结|概括|归纳|比较|对比|权衡|架构|方案|研究|调查|综述|审查|评审|诊断|为何|为什么|如何|怎么|怎样|哪里|在哪|何处|何时|什么时候|是否|是什么|有哪些|逻辑|原理|机制|报错原因|缺陷|漏洞|\b(?:explain|analy[sz]e|summari[sz]e|compare|review|inspect|diagnose|investigate|research|architecture|logic|implementation|semantics|why|where|when|which|what\s+(?:is|are|does)|how\s+(?:to|is|are|does|do|can|should)|find\s+(?:bugs?|errors?))\b";
const EXECUTION: &str = r"(?i)读取|读回|回读|读出|打开|查看|列出|写入|写到|写出|保存|创建|新建|追加|复制|移动|重命名|删除|执行|运行|启动|安装|构建|编译|终端|命令行|\b(?:read|open|list|write|save|create|append|copy|move|rename|delete|execute|run|launch|install|build|compile|terminal|shell)\b";
const EXTERNAL: &str = r"(?i)https?://|网页|网站|网址|浏览器|联网|上网|天气|新闻|\b(?:web|websites?|browser|online|internet|weather|news)\b";
const DETAIL: &str = r"(?i)取消|中断|恢复|重试|超时|队列|并发|绑定|缓存|索引|权限|审批|持久化|保存|存储|写入|序列化|分块|分词|嵌入|重排|上下文|引用|来源|配置|设置|生命周期|\b(?:cancel\w*|abort\w*|retry|retries|timeout|queue\w*|concurren\w*|binding|cache\w*|index\w*|permission\w*|approval|persist\w*|stor\w*|serializ\w*|chunk\w*|tokeniz\w*|embedding\w*|rerank\w*|context|reference\w*|configuration|settings|lifecycle)\b";
const CODE: &str = r"(?i)代码|源码|源文件|仓库|代码库|调用方|调用链|类型定义|单元测试|\b(?:source\s+code|source\s+files?|repository|repo|codebase|unit\s+tests?)\b|\b(?:explain|analy[sz]e|review|inspect|find|search|fix|modify|refactor)\b.{0,48}\bcode\b";
const DOCUMENT: &str = r"(?i)论文|文档|知识库|章节|笔记|\b(?:documentation|documents?|docs?|manuals?|chapters?)\b|\b(?:papers?)\s+(?:methods?|results?|experiments?|findings?)\b|\b(?:explain|analy[sz]e|review|read|summari[sz]e|compare)\b.{0,48}\b(?:papers?|notes?)\b|\b(?:according\s+to|based\s+on|from|in)\s+(?:(?:the|this|that|these|those|a)\s+)?papers?\b";
const DECLARATION: &str = r"(?i)(?:函数|方法|符号|类型).{0,12}(?:定义|实现|调用方|引用)|(?:查找|找到|定位).{0,40}定义|\b(?:find|locate|go\s+to)\b.{0,48}\b(?:definition|declaration)\b|\b(?:function|method|symbol|type)\b.{0,24}\b(?:definition|implementation|references|callers)\b";
const CHANGE: &str = r"(?i)修改|修复|重构|优化|实现|生成|写入|编辑|\b(?:fix|modify|refactor|implement|optimi[sz]e|generate|write|edit)\b";

pub(super) fn pattern(pattern: &'static str) -> Regex {
    static PATTERNS: OnceLock<Mutex<HashMap<&'static str, Regex>>> = OnceLock::new();
    let mut patterns = PATTERNS
        .get_or_init(|| Mutex::new(HashMap::new()))
        .lock()
        .expect("pure regex cache");
    patterns
        .entry(pattern)
        .or_insert_with(|| {
            Regex::new(
                &pattern
                    .replace(r"\b", r"(?-u:\b)")
                    .replace(r"\w", r"[A-Za-z0-9_]"),
            )
            .expect("static retrieval signal")
        })
        .clone()
}

fn matches(pattern_text: &'static str, text: &str) -> bool {
    pattern(pattern_text).is_match(text)
}
fn string<'a>(value: &'a Value, key: &str) -> &'a str {
    value[key].as_str().unwrap_or("")
}
fn active(query: &Value) -> &str {
    string(query, "activeText")
}
fn paths(query: &Value) -> Vec<&Value> {
    query["paths"]
        .as_array()
        .map_or(Vec::new(), |items| items.iter().collect())
}
fn observed_path_domain(query: &Value, path: &str) -> String {
    if query["explicitPath"].as_str() == Some(path) {
        return string(query, "explicitPathDomain").to_owned();
    }
    if let Some(reference) = paths(query)
        .into_iter()
        .find(|reference| normalize_path(string(reference, "value")) == normalize_path(path))
    {
        return string(reference, "domain").to_owned();
    }
    if let Some(domain) = query["intentPathDomain"].as_str() {
        return domain.to_owned();
    }
    "mixed".to_owned()
}
fn context_domain(context: &Value) -> &'static str {
    if matches!(
        context["domain"].as_str(),
        Some("code" | "knowledge" | "mixed")
    ) {
        return match context["domain"].as_str() {
            Some("code") => "code",
            Some("knowledge") => "knowledge",
            _ => "mixed",
        };
    }
    let query = &context["query"];
    let code =
        matches(CODE, active(query)) || paths(query).iter().any(|path| path["domain"] == "code");
    let document = matches(DOCUMENT, active(query))
        || paths(query)
            .iter()
            .any(|path| path["domain"] == "knowledge");
    if code == document {
        "mixed"
    } else if code {
        "code"
    } else {
        "knowledge"
    }
}

fn normalize_path(path: &str) -> String {
    let path = path.replace('\\', "/");
    let mut path = path.as_str();
    while path.starts_with("./") {
        path = &path[2..];
    }
    let mut normalized = String::new();
    let mut previous_slash = false;
    for character in path.chars() {
        if character != '/' || !previous_slash {
            normalized.push(character);
        }
        previous_slash = character == '/';
    }
    normalized.to_lowercase()
}

fn is_symbol_shape(value: &str) -> bool {
    matches(
        r"[a-z\d][A-Z]|[A-Z][a-z]+[A-Z]|[A-Za-z0-9_]_[A-Za-z0-9_]|\.",
        value,
    )
}

/// Treat language clues as preferences; exclude domains only for explicit constraints.
/// 自然语言线索仅影响偏好，显式约束才排除检索范围。
pub fn interpret(query: &Value, options: &Value, context: &Value, relation: &Value) -> Value {
    let text = active(query);
    let mut clues = Vec::new();
    if let Some(excluded) = query["excludedClauses"].as_array() {
        for clause in excluded {
            clues.push(json!({"kind":"rejected-condition","value":clause["text"],"strength":"weak","origin":"user","basis":clause["basis"],"startOffset":clause["startOffset"],"endOffset":clause["endOffset"]}));
        }
    }
    let code_source = matches(CODE, text);
    let document_source = matches(DOCUMENT, text);
    let mut unique_paths = Vec::new();
    let mut seen = HashSet::new();
    for path in paths(query) {
        if seen.insert(string(path, "value")) {
            unique_paths.push(path);
        }
    }
    let explicit_domain = options.get("domain").is_some();
    let explicit_path = options.get("path").is_some();
    let explicit_symbol = options.get("symbol").is_some();
    for (key, is_explicit) in [
        ("domain", explicit_domain),
        ("path", explicit_path),
        ("symbol", explicit_symbol),
    ] {
        if is_explicit {
            clues.push(
                json!({"kind":key,"value":options[key],"strength":"strong","origin":"caller"}),
            );
        }
    }
    for path in &unique_paths {
        clues.push(json!({"kind":"path","value":path["value"],"strength":"strong","origin":"user","verified":false}));
    }
    let mut path = options.get("path").cloned();
    if !explicit_path && unique_paths.len() == 1 {
        path = Some(unique_paths[0]["value"].clone());
    }
    if code_source {
        clues.push(
            json!({"kind":"domain-candidate","value":"code","strength":"weak","origin":"user"}),
        );
    }
    if document_source {
        clues.push(json!({"kind":"domain-candidate","value":"knowledge","strength":"weak","origin":"user"}));
    }
    let quoted = pattern(
        r#"([`"'“])([A-Za-z_$][A-Za-z0-9_$]*(?:\.[A-Za-z_$][A-Za-z0-9_$]*)*)(\(\))?[`"'”]"#,
    )
    .captures_iter(text)
    .map(|capture| {
        (
            capture[1].to_owned(),
            capture[2].to_owned(),
            capture.get(3).is_some(),
        )
    })
    .collect::<Vec<_>>();
    let identifiers = pattern(r"[A-Za-z_$][A-Za-z0-9_$]*(?:\.[A-Za-z_$][A-Za-z0-9_$]*)*")
        .find_iter(text)
        .map(|value| value.as_str().to_owned())
        .collect::<Vec<_>>();
    let path_part = |value: &str| {
        unique_paths.iter().any(|path| {
            string(path, "value")
                .to_lowercase()
                .contains(&value.to_lowercase())
        }) || path
            .as_ref()
            .and_then(Value::as_str)
            .is_some_and(|path| path.to_lowercase().contains(&value.to_lowercase()))
    };
    let code_path = path
        .as_ref()
        .and_then(Value::as_str)
        .is_some_and(|path| observed_path_domain(query, path) == "code");
    let mut named = Vec::new();
    if matches(
        r"(?i)函数|方法|符号|类型|\b(?:function|method|symbol|type)\b",
        text,
    ) {
        for named_pattern in [
            r#"(?i)(?:函数|方法|符号|类型|\b(?:function|method|symbol|type)\b)\s*[`"'“]?([A-Za-z_$][A-Za-z0-9_$]*(?:\.[A-Za-z_$][A-Za-z0-9_$]*)*)"#,
            r#"(?i)[`"'“]?([A-Za-z_$][A-Za-z0-9_$]*(?:\.[A-Za-z_$][A-Za-z0-9_$]*)*)[`"'”]?\s*(?:函数|方法|符号|类型|\s+\b(?:function|method|symbol|type)\b)"#,
        ] {
            for capture in pattern(named_pattern).captures_iter(text) {
                let value = capture[1].to_owned();
                if !path_part(&value)
                    && (is_symbol_shape(&value)
                        || quoted.iter().any(|(_, symbol, _)| symbol == &value))
                {
                    named.push(value);
                }
            }
        }
    }
    let named_declaration = !named.is_empty() && (code_source || code_path);
    let declaration_request = (matches(DECLARATION, text) || named_declaration)
        && (!document_source || code_source || code_path);
    let declaration_target = declaration_request
        && (!named.is_empty()
            || !quoted.is_empty()
                && matches(r"(?i)函数|符号|类型|\b(?:function|symbol|type)\b", text));
    let named_target = declaration_target
        && (code_source
            || code_path
            || matches(r"(?i)函数|符号|类型|\b(?:function|symbol|type)\b", text));
    let quoted_target = quoted.iter().find(|(quote, value, invocation)| {
        !path_part(value)
            && (named_target
                || declaration_request
                    && quote == "`"
                    && matches(r"(?i)定义|\b(?:definition|declaration)\b", text)
                || code_path
                    && quote == "`"
                    && matches(r"(?i)查找|找到|定位|\b(?:find|locate)\b", text)
                || !document_source && quote == "`" && (*invocation || value.contains('.')))
    });
    let symbol = if explicit_symbol {
        options.get("symbol").cloned()
    } else {
        quoted_target
            .map(|(_, symbol, _)| json!(symbol))
            .or_else(|| {
                if named_target {
                    named.first().map(|symbol| json!(symbol))
                } else {
                    None
                }
            })
    };
    if !explicit_symbol {
        if let Some(symbol) = &symbol {
            clues.push(json!({"kind":"symbol","value":symbol,"strength":"strong","origin":"user"}));
        }
    }
    let mut identifier_candidates = Vec::new();
    let mut seen = HashSet::new();
    for value in quoted
        .iter()
        .map(|(_, value, _)| value)
        .chain(identifiers.iter().filter(|value| is_symbol_shape(value)))
    {
        if seen.insert(value) {
            identifier_candidates.push(value);
        }
    }
    for value in identifier_candidates {
        if !path_part(value) && symbol.as_ref().and_then(Value::as_str) != Some(value) {
            clues.push(
                json!({"kind":"symbol-candidate","value":value,"strength":"weak","origin":"user"}),
            );
        }
    }
    for found in pattern(r"(?i)方法|函数|符号|\b(?:method|function|symbol)\b").find_iter(text)
    {
        clues.push(json!({"kind":"ambiguous-term","value":found.as_str(),"strength":"weak","origin":"user"}));
    }
    let mut preferred = options.get("preferredDomain").cloned();
    if !explicit_domain {
        let code_target = code_source
            || declaration_target
            || symbol.is_some()
            || unique_paths.iter().any(|path| path["domain"] == "code")
            || code_path;
        let document_target = document_source
            || unique_paths
                .iter()
                .any(|path| path["domain"] == "knowledge")
            || path
                .as_ref()
                .and_then(Value::as_str)
                .is_some_and(|path| observed_path_domain(query, path) == "knowledge");
        if code_target != document_target && preferred.is_none() {
            preferred = Some(json!(if code_target { "code" } else { "knowledge" }));
        }
        let related = relation["allowsInheritance"] == true
            || matches(ANALYSIS, text) && matches(DETAIL, text);
        if !code_target
            && !document_target
            && related
            && !matches!(
                relation["type"].as_str(),
                Some("topic-switch" | "correction")
            )
            && !matches(EXTERNAL, text)
        {
            let domain = context_domain(context);
            if domain != "mixed" {
                if preferred.is_none() {
                    preferred = Some(json!(domain));
                }
                clues.push(json!({"kind":"domain-preference","value":domain,"strength":"weak","origin":"task-context"}));
            }
        }
    }
    let mut intent =
        json!({"domain":if explicit_domain {options["domain"].clone()} else {json!("mixed")}});
    if let Some(path) = path {
        intent["path"] = if let Some(path) = path.as_str() {
            json!(normalize_path(path))
        } else {
            path
        };
    }
    if let Some(symbol) = symbol {
        intent["symbol"] = symbol;
    }
    if let Some(preferred) = preferred {
        intent["preferredDomain"] = preferred;
    }
    let mut result = json!({"intent":intent,"domainConstraint":if explicit_domain {"explicit"} else {"none"},"clues":clues,"taskRelation":relation,"activeText":text,
        "excludedClauses":query["excludedClauses"],"queryCorrections":query["provenance"]});
    if let Some(correction) = query["provenance"]
        .as_array()
        .and_then(|items| items.first())
    {
        result["queryCorrection"] = correction.clone();
    }
    result
}

fn direct_execution(query: &Value) -> bool {
    let text = string(query, "withoutPaths");
    matches(EXECUTION, text) && !matches(KNOWLEDGE, text) && !matches(ANALYSIS, text)
}

fn requested_operation(text: &str) -> &'static str {
    if matches(
        r"(?i)所有.{0,12}(?:引用|调用方)|穷举.{0,12}引用|\b(?:all\s+(?:references|callers)|enumerate\s+references)\b",
        text,
    ) {
        "references"
    } else if matches(
        r"(?i)(?:查找|找到|定位).{0,12}定义|\b(?:find|go\s+to|locate)\s+(?:the\s+)?definition\b",
        text,
    ) {
        "definition"
    } else {
        "search"
    }
}

fn changed_text(query: &Value) -> String {
    pattern(r"[，。；,;!?！？\n]+").split(string(query,"instructionText")).filter(|clause| !matches(r"(?i)^(?:(?:请|先|暂时|目前|这次|我|你)\s*|(?:please|for now|currently)\s+)*(?:不是|并非|不要|不用|不必|无需|不能|不许|别|勿|禁止|避免|do\s+not\b|don['’]t\b|never\b|not\b)",clause.trim())).collect::<Vec<_>>().join("，")
}

fn code_validation(query: &Value, intent: &Value) -> bool {
    let text = changed_text(query);
    matches(CHANGE, &text)
        && (intent["domain"] == "code"
            || matches(CODE, &text)
            || !string(intent, "symbol").is_empty()
            || observed_path_domain(query, string(intent, "path")) == "code"
            || query["changePaths"]
                .as_array()
                .is_some_and(|paths| paths.iter().any(|path| path["domain"] == "code")))
}

fn take_utf16(text: &str, maximum: usize) -> String {
    let units = text.encode_utf16().collect::<Vec<_>>();
    let mut end = maximum.min(units.len());
    if end > 0 && (0xd800..=0xdbff).contains(&units[end - 1]) {
        end -= 1;
    }
    String::from_utf16_lossy(&units[..end])
}

pub fn plan(args: &Value) -> Value {
    let query = &args["query"];
    let text = active(query);
    let original = string(query, "originalText");
    let current = interpret(query, &json!({}), &Value::Null, &query["taskRelation"]);
    let mut relation = query["taskRelation"].clone();
    let greeting = matches(GREETING, text.trim());
    let small_talk = matches(SMALL_TALK, text.trim());
    let direct = direct_execution(query);
    let explicit_file = !paths(query).is_empty();
    let instruction = string(query, "instructionText");
    let name_discussion = matches(
        r"(?i)文件(?:名|名称)|\b(?:file\s*names?|filenames?)\b",
        instruction,
    ) && matches(
        r"(?i)命名|名字|名称|风格|\b(?:naming|names?|style|convention)\b",
        instruction,
    ) && !query["instructionHasPaths"].as_bool().unwrap_or(false)
        && !matches(KNOWLEDGE, instruction)
        && !matches(CODE, instruction)
        && !matches(
            r"(?i)读取|查看|内容|实现|定义|源码|源文件|\b(?:read|contents?|implementation|definition|source)\b",
            instruction,
        );
    let local = !name_discussion
        && (matches(LOCAL, text)
            || matches(KNOWLEDGE, text)
            || matches(CODE, text)
            || matches(DOCUMENT, text)
            || !string(&current["intent"], "symbol").is_empty()
            || explicit_file);
    let domain = context_domain(&args["context"]);
    let task_reference = !matches!(
        relation["type"].as_str(),
        Some("topic-switch" | "correction")
    ) && domain != "mixed"
        && !matches(EXTERNAL, text)
        && matches(ANALYSIS, text)
        && (matches(DETAIL, text) || relation["allowsInheritance"] == true);
    if task_reference && !local && relation["type"] == "new" {
        relation = json!({"type":"uncertain","allowsInheritance":false,"reason":"technical-detail-may-relate-to-current-task"});
    }
    let followup = !greeting
        && !small_talk
        && !direct
        && !name_discussion
        && relation["allowsInheritance"] == true;
    let mut previous: Option<&Value> = None;
    if followup {
        if let Some(history) = args["history"].as_array() {
            for message in history.iter().rev() {
                if message["role"] != "user"
                    || message["status"]
                        .as_str()
                        .is_some_and(|status| !status.is_empty() && status != "completed")
                    || string(&message["query"], "originalText").trim() == original.trim()
                {
                    continue;
                }
                let candidate = &message["query"];
                let text = active(candidate);
                if direct_execution(candidate) {
                    break;
                }
                if matches(LOCAL, text)
                    || matches(KNOWLEDGE, text)
                    || matches(CODE, text)
                    || matches(DOCUMENT, text)
                    || !paths(candidate).is_empty()
                {
                    previous = Some(message);
                    break;
                }
                if candidate["taskRelation"]["allowsInheritance"] != true {
                    break;
                }
            }
        }
    }
    if let Some(message) = previous {
        let prior = interpret(
            &message["query"],
            &json!({}),
            &Value::Null,
            &message["query"]["taskRelation"],
        );
        let now = &current["intent"];
        let prior = &prior["intent"];
        let conflict = ["preferredDomain", "path", "symbol"].iter().any(|key| {
            now.get(key).is_some() && prior.get(key).is_some() && now[key] != prior[key]
        });
        if conflict {
            previous = None;
            relation = json!({"type":"uncertain","allowsInheritance":false,"reason":"current-target-conflicts-with-prior-context"});
        }
    }
    let retrieve = !greeting && !small_talk && !direct && (local || previous.is_some());
    let mut task = if explicit_file {
        "file"
    } else if matches(
        r"(?i)记忆|回忆|历史|之前|上次|讨论|history|remember|recall|previous",
        text,
    ) {
        "recall"
    } else {
        "lookup"
    };
    if matches(
        r"(?i)比较|对比|权衡|架构|方案|全面|综述|调查|研究|compare|research|trade.?off",
        text,
    ) {
        task = "research";
    }
    if direct {
        task = "execution";
    }
    let default_tokens = match task {
        "file" => 16384,
        "research" => 32768,
        "execution" => 0,
        _ => 8192,
    };
    let evidence_tokens = args.get("maximumTokens").map_or(default_tokens, |tokens| {
        tokens.as_f64().map_or(0, |tokens| {
            tokens.floor().max(0.0).min(default_tokens as f64) as u64
        })
    });
    let addition_budget = 256usize.min(4000usize.saturating_sub(text.encode_utf16().count() + 1));
    let prior_query = previous.map_or(String::new(), |message| {
        take_utf16(active(&message["query"]).trim(), addition_budget)
    });
    let current_query = if text.is_empty() { original } else { text };
    let final_query = if retrieve && !prior_query.is_empty() {
        format!("{current_query}\n{prior_query}")
    } else {
        current_query.to_owned()
    };
    let context = previous.map_or(
        args["context"].clone(),
        |message| json!({"query":message["query"]}),
    );
    let interpretation = interpret(query, &json!({}), &context, &relation);
    let intent = &interpretation["intent"];
    let read = task == "research" || task == "file";
    let operation = requested_operation(text);
    let code_roles = json!(["implementation", "callers", "configuration", "tests"]);
    let source_roles = json!(["source", "conditions"]);
    let mut additions = Vec::new();
    if retrieve && !prior_query.is_empty() {
        let mut addition =
            json!({"text":prior_query,"origin":"history","basis":"explicit-continuation"});
        if let Some(id) = previous
            .and_then(|message| message.get("id"))
            .filter(|id| !id.is_null())
        {
            addition["historyId"] = id.clone();
        }
        additions.push(addition);
    }
    let replacements = query["provenance"]
        .as_array()
        .map_or(Vec::new(), |corrections| {
            corrections
                .iter()
                .map(|correction| {
                    let mut correction = correction.clone();
                    correction["origin"] = json!("user");
                    correction
                })
                .collect::<Vec<_>>()
        });
    let reason = if greeting {
        "greeting"
    } else if small_talk {
        "small-talk"
    } else if direct {
        "direct-execution"
    } else if previous.is_some() {
        "prior-local-request"
    } else if local {
        "local-reference"
    } else if task_reference {
        "task-context-reference"
    } else {
        "no-local-reference"
    };
    let mut result = json!({"shouldRetrieve":retrieve,"taskType":task,"query":final_query,"originalQuery":original,"evidenceTokens":evidence_tokens,"requiresSourceRead":read,
        "retrievalIntent":intent,"domainConstraint":interpretation["domainConstraint"],"clues":interpretation["clues"],"taskRelation":relation,
        "queryDerivation":{"originalQuery":original,"additions":additions,"replacements":replacements,"preservedOriginal":true},"operation":operation,
        "supportedOperations":["search","read","indexed-definition"],"operationSupported":operation!="references",
        "evidenceRoles":if intent["domain"]=="code" || code_validation(query,intent) {code_roles.clone()} else {source_roles.clone()},
        "suggestedEvidenceRoles":if intent["preferredDomain"]=="code" {code_roles} else {source_roles},"reason":reason,"rewritten":final_query!=original});
    if let Some(fields) = intent.as_object() {
        for (key, value) in fields {
            result[key] = value.clone();
        }
    }
    result
}

pub fn exact_target(item: &Value, intent: &Value) -> bool {
    let path = string(intent, "path");
    let symbol = string(intent, "symbol");
    if path.is_empty() && symbol.is_empty() {
        return false;
    }
    let item_path = string(item, "exactPath");
    let path_match = path.is_empty()
        || item_path == path.to_lowercase()
        || item_path.ends_with(&format!("/{}", path.to_lowercase()));
    let symbol_match = symbol.is_empty()
        || ["symbolName", "qualifiedName"].iter().any(|key| {
            item["structure"][key]
                .as_str()
                .is_some_and(|value| value == symbol || value.ends_with(&format!(".{symbol}")))
        });
    item["exactTargetMatch"] == true || path_match && symbol_match
}

fn acquisition(args: &Value) -> Value {
    let query = &args["query"];
    let items = args["items"].as_array().cloned().unwrap_or_default();
    let mut intent = args["intent"].clone();
    let assessment = &args["assessment"];
    if string(&intent, "symbol").is_empty()
        && matches(
            r"(?i)定义|\b(?:definition|declaration)\b",
            string(query, "originalText"),
        )
    {
        let interpretation = interpret(query, &json!({}), &Value::Null, &query["taskRelation"]);
        for clue in interpretation["clues"]
            .as_array()
            .into_iter()
            .flatten()
            .filter(|clue| clue["kind"] == "symbol-candidate")
        {
            let value = string(clue, "value");
            if !string(query, "originalText").contains(&format!("`{value}`")) {
                continue;
            }
            let mut candidate = intent.clone();
            candidate["symbol"] = json!(value);
            if items.iter().any(|item| {
                let mut item = item.clone();
                item["exactTargetMatch"] = json!(false);
                exact_target(&item, &candidate)
            }) {
                intent = candidate;
                break;
            }
        }
    }
    let exact = items
        .iter()
        .any(|item| item["exactTargetMatch"] == true || exact_target(item, &intent));
    let targeted =
        !string(&intent, "path").is_empty() || !string(&intent, "symbol").is_empty() || exact;
    let embedding =
        args["embeddingStatus"]["state"] == "ready" && args["embeddingStatus"]["loaded"] != false;
    let semantic = args
        .get("semanticEnabled")
        .is_none_or(|value| value != &Value::Bool(false));
    let missing = assessment["missingEvidence"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    let mut channels = if targeted {
        vec!["lexical", "structure"]
    } else {
        vec!["lexical"]
    };
    if semantic && embedding && !targeted {
        channels.push("vector");
    }
    let read =
        !items.is_empty() && (assessment["requiresSourceRead"] == true || !missing.is_empty());
    let no_progress = args["reused"] == true || args["newEvidenceCount"].as_u64() == Some(0);
    let exhausted = args["remainingSearches"].as_f64().unwrap_or(1.0) <= 0.0;
    let pending = args["indexingPending"] == true;
    let next = if read {
        "read-source"
    } else if !items.is_empty() {
        "evaluate-support-and-answer"
    } else if exhausted {
        "state-unresolved-gap"
    } else if pending {
        "direct-search-or-read"
    } else {
        "search-specific-gap"
    };
    let verify = matches(CHANGE, &changed_text(query));
    let checks = if verify {
        vec![
            "re-read-current-source",
            if code_validation(query, &args["intent"]) {
                "run-relevant-validation"
            } else {
                "check-deliverable-against-requirements"
            },
        ]
    } else {
        Vec::new()
    };
    json!({"channels":channels,"shouldEmbed":semantic && embedding && !targeted,"shouldRerank":false,
        "rankingMethod":"rank-rrf-structured","maximumSupplementalSearches":2,"preferredTool":if targeted {"filesystem.search"} else {"knowledge.search"},
        "missingInformation":args["gap"],"missingEvidence":missing,"next":next,"shouldContinueSearch":!exhausted && !no_progress && !read && items.is_empty(),
        "requiresVerification":verify,"requiredChecks":checks,"coverage":if pending {"partial-index"} else {"bounded-candidates"},"sufficiency":"not-evaluated"})
}

fn priority_indices(args: &Value) -> Value {
    let sources = args["sources"].as_array().cloned().unwrap_or_default();
    let matches = |path: &str, targets: &Value| {
        targets.as_array().is_some_and(|targets| {
            targets.iter().filter_map(Value::as_str).any(|target| {
                let target = target.to_lowercase();
                path == target
                    || path.starts_with(&format!("{target}/"))
                    || path.ends_with(&format!("/{target}"))
            })
        })
    };
    let mut indices = (0..sources.len()).collect::<Vec<_>>();
    indices.sort_by_key(|index| {
        let source=&sources[*index];let path=string(source,"relativePath");let absolute=string(source,"absolutePath");
        if matches(path,&args["priorityPaths"]) || !absolute.is_empty() && matches(absolute,&args["priorityPaths"]) {0}
        else if matches(path,&args["recentPaths"]) || !absolute.is_empty() && matches(absolute,&args["recentPaths"]) {1}
        else if pattern(r"(?i)(?:^|/)(?:readme|agents)(?:\.|$)|(?:^|/)(?:tests?|src)(?:/|$)|\.(?:cs|[cm]?js|[cm]?ts|py|rs|go|json|ya?ml|toml)$").is_match(path) {2} else {3}
    });
    json!(indices)
}

pub fn apply(operation: &str, args: &Value) -> Result<Value, String> {
    Ok(match operation {
        "retrieval.greeting" => json!(matches(GREETING, string(args, "text").trim())),
        "retrieval.query-interpret" => interpret(
            &args["query"],
            &args["options"],
            &args["context"],
            args.get("taskRelation")
                .unwrap_or(&args["query"]["taskRelation"]),
        ),
        "retrieval.query-plan" => plan(args),
        "retrieval.exact-target" => json!(exact_target(&args["item"], &args["intent"])),
        "retrieval.acquisition-plan" => acquisition(args),
        "retrieval.source-priorities" => priority_indices(args),
        _ => return Err("AGENT_CORE_UNKNOWN_OPERATION".into()),
    })
}

use serde_json::{Value, json};
use unicode_normalization::UnicodeNormalization;

use super::intent::{analyze_clauses, file_references, js_trim, matches, pattern, task_relation};

// Native calculations retain descriptor indices; opt-in scores allow relevance-preserving experience ranking.
// 原生计算保留描述符索引；按需附带相关性分数，经验排序不能改变原 schema 或 wire 身份。
fn normalized(value: &Value, maximum: usize) -> String {
    let Some(text) = value.as_str() else {
        return String::new();
    };
    let units = text.encode_utf16().take(maximum).collect::<Vec<_>>();
    js_trim(&String::from_utf16_lossy(&units).nfkc().collect::<String>()).to_lowercase()
}

fn workspace_terminal_topic(text: &str) -> bool {
    matches(
        r"(?:右侧|右边|侧栏|工作区|内置|嵌入).{0,16}(?:终端|命令行)|(?:终端|命令行).{0,16}(?:右侧|右边|侧栏|工作区|内置)|\b(?:sidebar|workspace|embedded)\s+terminal\b",
        text,
    )
}

/// Match authoring intent and format together; code documentation and file reads are not Office generation.
/// 同时匹配创作意图与格式，代码文档查询或读取文件不会被误判为生成 Office 文档。
pub(super) fn authoring_topics(query: &str) -> Vec<&'static str> {
    let text = query.to_lowercase();
    if !matches(r"生成|创建|写|制作|导出|保存|整理成|做成|\b(?:create|generate|write|export|build|make|save)\b", &text) {
        return vec![];
    }
    let mut topics = Vec::new();
    if matches(r"\b(?:docx|word|pdf)\b|文档|报告|总结|论文|合同", &text) {
        topics.push("document-authoring");
    }
    if matches(r"\b(?:pptx?|powerpoint|slides?|presentation)\b|幻灯片|演示文稿|汇报演示", &text) {
        topics.push("slides-authoring");
    }
    if matches(r"\b(?:xlsx?|excel|spreadsheet|workbook|csv|tsv)\b|电子表格|工作簿|数据表", &text) {
        topics.push("spreadsheets-authoring");
    }
    if matches(r"\b(?:svg|html|mermaid|diagram)\b|流程图|架构图|关系图|可视化", &text) {
        topics.push("visual-artifacts");
    }
    topics
}

pub(super) fn category(tool: &Value) -> &'static str {
    let name = normalized(&tool["name"], 256);
    let description = normalized(&tool["description"], 2000);
    if name.starts_with("computer.") {
        return "computer";
    }
    if name.starts_with("terminal.host.") || name.starts_with("terminal.workspace.") {
        return "host-terminal";
    }
    if name == "terminal.run" {
        return "sandbox-terminal";
    }
    if name == "web.fetch" {
        return "web-fetch";
    }
    if matches(
        r"playwright|chrome[-_.]?devtools|puppeteer|(?:^|[._-])browser(?:[._-]|$)",
        &name,
    ) || matches(r"browser automation|browser debugging", &description)
    {
        return "browser";
    }
    if matches(
        r"web[-_]?search|search[-_]?web|search[-_]?news|news[-_]?search",
        &name,
    ) || matches(
        r"public web search|search (?:the )?(?:web|internet)",
        &description,
    ) {
        return "web-search";
    }
    if name.starts_with("filesystem.") || ["document.read", "document.create", "image.read", "image.retain", "attachment.read", "attachment.export", "artifact.open", "media.transcribe"].contains(&name.as_str()) {
        return "filesystem";
    }
    "other"
}

fn query_terms(query: &str) -> Vec<String> {
    let mut terms = Vec::new();
    let mut append = |word: String| {
        if !terms.contains(&word) {
            terms.push(word);
        }
    };
    for term in pattern(r"[a-z0-9]+(?:[_-][a-z0-9]+)*").find_iter(query) {
        append(term.as_str().to_owned());
    }
    drop(append);
    for phrase in pattern(r"\p{Han}{2,}").find_iter(query) {
        let phrase = phrase.as_str();
        if !terms.iter().any(|term| term == phrase) {
            terms.push(phrase.to_owned());
        }
        let characters = phrase.chars().collect::<Vec<_>>();
        for pair in characters.windows(2) {
            if terms.len() >= 32 {
                break;
            }
            let word = pair.iter().collect::<String>();
            if !terms.contains(&word) {
                terms.push(word);
            }
        }
    }
    terms.truncate(32);
    terms
}

fn aliases(tool: &Value) -> String {
    let name = normalized(&tool["name"], 256);
    let category = category(tool);
    let action = name.rsplit('.').next().unwrap_or("");
    if name == "work.folder.bind" {
        return "work project folder directory bind associate attach mount ongoing 长期工作 工作文件夹 工作目录 关联目录 绑定目录 挂载文件夹".to_owned();
    }
    if name == "document.read" {
        return "document read parse extract word docx doc pdf ppt pptx xlsx markdown image OCR 文档 读取文档 解析文档 正文 页码 图片 文字识别 表格 幻灯片".to_owned();
    }
    if name == "image.read" {
        return "image photo picture read view inspect analyze pixels local png jpeg jpg webp gif crop vision 图片 图像 照片 看图 读图 分析图片 查看图片 本地图片 裁剪 局部 像素 颜色 画面 构图".to_owned();
    }
    if name == "image.retain" {
        return "image screenshot retain keep preserve evidence result receipt 图片 截图 保留 保存证据 关键图像 回执".to_owned();
    }
    if name == "document.create" {
        return "generate create write export document docx word pdf pptx powerpoint presentation slides xlsx excel spreadsheet workbook 生成文档 创建文档 写文档 导出 报告 总结 幻灯片 演示文稿 电子表格 工作簿".to_owned();
    }
    if name == "attachment.read" {
        return "attachment upload attached document image read paginate 附件 上传文件 读取附件 正文 分页".to_owned();
    }
    if name == "attachment.export" {
        return "attachment upload original bytes export copy processing script crop pixelate 附件 原图 原件 导出 下载 附件落盘 复制附件 图像处理 图片处理 裁剪 像素化 拼豆 脚本处理".to_owned();
    }
    if name == "artifact.open" {
        return "artifact preview display open sidebar file document markdown mermaid pdf word powerpoint website browser 产物 文件 预览 展示 侧栏 文档 网页".to_owned();
    }
    if category == "computer" {
        return format!(
            "computer desktop local host 本机 本地 桌面 桌面控制 {}",
            computer_alias(name.strip_prefix("computer.").unwrap_or(""))
        );
    }
    if category == "host-terminal" {
        if name.starts_with("terminal.workspace.") {
            return format!("sidebar workspace embedded interactive terminal command powershell cmd right pane 右侧终端 右边终端 工作区终端 内置终端 交互终端 命令 执行 输入 输出 会话 {}", match action {
                "open" => "open launch 打开 创建",
                "list" => "list existing 列出 已有",
                "read" => "read output 读取 查看 输出",
                "write" => "write input run command 输入 运行命令 执行命令",
                "close" => "close stop 关闭 停止",
                _ => "",
            });
        }
        let action = match action {
            "start" => "start launch 启动 运行",
            "read" => "read inspect poll output 读取 查看 输出 状态",
            "stop" => "stop cancel terminate 停止 关闭 终止 取消",
            _ => "",
        };
        let device = if name == "terminal.host.run" {
            " device status current network my ip address public ip proxy dns adapter interfaces ipconfig tasklist processes listening port netstat get-nettcpconnection 本机状态 我的ip 当前网络 网络配置 代理设置 代理配置 dns设置 dns配置 dns服务器 网卡 网络适配器 本机进程 进程列表 端口占用 监听端口 哪个程序占端口"
        } else {
            ""
        };
        return format!(
            "host terminal local terminal visible terminal background process job cmd powershell conda 本机终端 本地终端 可见终端 显示终端 终端窗口 宿主终端 本机命令 本地命令 命令提示符 后台进程 后台任务 监控进程 {action}{device}"
        );
    }
    if category == "sandbox-terminal" {
        return "sandbox terminal cmd 沙箱终端 隔离终端 运行命令 执行命令".to_owned();
    }
    if category == "web-search" {
        return "web search internet 搜索 联网 搜索网页 查资料 查证".to_owned();
    }
    if category == "web-fetch" {
        return "web fetch public webpage read url 页面 网页 网站 阅读网页 读取网页 网页内容 网址 获取正文".to_owned();
    }
    if category == "filesystem" {
        return format!(
            "file filesystem 文件 文件夹 工作目录 {}",
            match action {
                "read" => "读取 查看",
                "list" => "列表 列出",
                "search" => "搜索 查找",
                "write" => "写入 创建 新建 保存",
                "edit" => "编辑 修改",
                "delete" => "删除",
                "stat" => "属性 信息",
                "mkdir" => "目录 创建文件夹",
                _ => "",
            }
        );
    }
    if category != "browser" {
        return String::new();
    }
    let mut value =
        "browser chrome devtools playwright 浏览器 网页 网站 网址 浏览器自动化".to_owned();
    for (condition, alias) in [
        (
            r"navigate|new_page|new[-_]tab",
            " navigate open visit 导航 打开 访问 网页 页面",
        ),
        (
            r"snapshot|read|content|evaluate[-_]script",
            " read inspect content 读取 查看 网页内容 页面内容 浏览器内容",
        ),
        (
            r"evaluate|script",
            " evaluate script javascript 脚本 执行脚本",
        ),
        (r"screenshot|capture", " screenshot capture 截图 截屏"),
        (r"click", " click 点击"),
        (
            r"type|fill",
            " type fill input enter text paste caret focused field 输入 键入 填写 填入 填进 粘贴 光标 输入框",
        ),
    ] {
        if matches(condition, &name) {
            value.push_str(alias);
        }
    }
    value
}

fn search_tools(arguments: &Value) -> Value {
    let descriptors = arguments["descriptors"]
        .as_array()
        .map(Vec::as_slice)
        .unwrap_or(&[]);
    let text = normalized(&arguments["query"], 2000);
    let terms = query_terms(&text);
    let device = device_state_topic(&text);
    let active = analyze_clauses(&text);
    let active_text = active["activeText"].as_str().unwrap_or(&text);
    let workspace_terminal = workspace_terminal_topic(active_text);
    let image_read = matches(r"(?:\.(?:png|jpe?g|webp|gif)\b|图片|图像|照片|\b(?:image|photo|picture)\b)", active_text)
        && matches(r"看图|读图|分析|读取|查看|辨认|识别|\b(?:read|inspect|analy[sz]e|describe|view)\b", active_text);
    // Exact capability names in the active request outrank generic words in long tasks or exclusions.
    // 当前有效请求中的完整能力名优先于长任务里的泛词；被明确排除的能力名不获得此优先级。
    let named_tools = pattern(r"[a-z0-9_-]+(?:\.[a-z0-9_-]+)+|k_[a-z0-9_]+_[a-f0-9]{8}")
        .find_iter(active_text)
        .map(|found| (found.as_str(), found.start()))
        .collect::<Vec<_>>();
    let has_file_target = file_references(active_text).as_array().is_some_and(|files| !files.is_empty());
    let file_read = has_file_target.then(|| pattern(r"读取|读入|阅读|\bread\b").find(active_text)).flatten();
    let file_write = has_file_target.then(|| pattern(r"写入|创建|新建|保存|\b(?:write|create|save)\b").find(active_text)).flatten();
    let identity = matches(r"^[a-z0-9_-]+(?:\.[a-z0-9_-]+)+$", &text);
    let wire_identity = matches(r"^k_[a-z0-9_]+_[a-f0-9]{8}$", &text);
    let mut selected = Vec::new();
    for (index, tool) in descriptors.iter().enumerate() {
        if tool["enabled"] == false || tool["available"] == false {
            continue;
        }
        if text.is_empty() {
            selected.push((index, 0));
            continue;
        }
        let name = normalized(&tool["name"], 256);
        let wire_name = normalized(&tool["wireName"], 256);
        if let Some((_, position)) = named_tools.iter().find(|(named, _)| *named == name || !wire_name.is_empty() && *named == wire_name) {
            selected.push((index, 1_000_000usize.saturating_sub(*position)));
            continue;
        }
        if identity && !name.contains(&text) {
            continue;
        }
        if wire_identity {
            if normalized(&tool["wireName"], 256) == text {
                selected.push((index, 100000));
            }
            continue;
        }
        let description = normalized(&tool["description"], 8000);
        let alias = aliases(tool);
        let mut score = if name == text {
            100000
        } else if name.contains(&text) {
            10000
        } else {
            0
        };
        if name == "document.create" && authoring_topics(&text).iter().any(|topic| *topic != "visual-artifacts") {
            score += 30_000;
        }
        if name == "image.read" && image_read { score += 30_000; }
        // A named file plus an explicit read/write action beats incidental local/keyboard vocabulary.
        // 明确文件目标及读写动作优先于偶然出现的本地或键盘词；此处只排序，不扩大执行权限。
        if let Some(action) = match name.as_str() {
            "filesystem.read" => file_read.as_ref(),
            "filesystem.write" => file_write.as_ref(),
            _ => None,
        } {
            score += 20_000usize.saturating_sub(action.start());
        }
        // A right-side command must keep its visible session instead of preferring hidden host diagnostics.
        // 右侧命令优先保留可见会话，不能被通用本机诊断排序改成隐藏的宿主终端。
        if workspace_terminal && name.starts_with("terminal.workspace.") {
            score += 6000;
        }
        if !workspace_terminal && device == Some("device") && name == "terminal.host.run" {
            score += 5000;
        }
        if description.contains(&text) || alias.contains(&text) {
            score += 1000;
        }
        let mut found = 0;
        for term in &terms {
            let in_name = name.contains(term);
            let in_alias = alias.contains(term);
            if in_name || in_alias || description.contains(term) {
                found += 1;
                score += if in_name {
                    40
                } else if in_alias {
                    24
                } else {
                    8
                };
            }
        }
        if found > 0 {
            score += found * 100 + if found == terms.len() { 300 } else { 0 };
        }
        if score > 0 {
            selected.push((index, score));
        }
    }
    if !text.is_empty() {
        selected.sort_by(|left, right| right.1.cmp(&left.1).then(left.0.cmp(&right.0)));
    }
    if arguments["includeScores"] == true {
        json!(selected.into_iter().map(|(index, score)| json!({"index":index,"score":score})).collect::<Vec<_>>())
    } else {
        json!(selected.into_iter().map(|item| item.0).collect::<Vec<_>>())
    }
}

fn browser_boundary(text: &str) -> Option<&'static str> {
    let projection = analyze_clauses(text);
    let text = projection["activeText"].as_str().unwrap();
    let local = pattern(r"(?:本机|本地|可见|我的|自己的).{0,16}(?:浏览器|chrome|edge|窗口)|\blocal\s+(?:browser|chrome|edge)\b").find_iter(text).last().map(|found| found.start());
    let remote = pattern(r"(?:云端|远程).{0,16}浏览器|\b(?:remote|cloud)\s+browser\b")
        .find_iter(text)
        .last()
        .map(|found| found.start());
    if local.is_some() || remote.is_some() {
        return Some(if local.is_some() && (remote.is_none() || local > remote) {
            "local"
        } else {
            "remote"
        });
    }
    if matches(
        r"(?:打开|启动|显示).{0,12}(?:chrome|edge|firefox|google)|\b(?:open|launch|show)\s+(?:the\s+)?(?:chrome|edge|firefox)\b",
        text,
    ) {
        Some("local")
    } else {
        None
    }
}

fn discovery_relation(text: &str, supplied: &Value) -> String {
    let relation = task_relation(text);
    let current = relation["type"].as_str().unwrap();
    if matches!(current, "topic-switch" | "correction") {
        return current.to_owned();
    }
    let supplied = supplied.as_str().or_else(|| supplied["type"].as_str());
    if let Some(
        kind @ ("continue" | "supplement" | "correction" | "topic-switch" | "switch" | "new"
        | "uncertain"),
    ) = supplied
    {
        kind.to_owned()
    } else {
        current.to_owned()
    }
}

pub(super) fn selection_signals(arguments: &Value) -> Value {
    let text = normalized(&arguments["message"], 2000);
    let projection = analyze_clauses(&text);
    let mut signals = current_signals(&text);
    let recent = arguments["historySignals"]
        .as_array()
        .map(|history| {
            history[history.len().saturating_sub(3)..]
                .iter()
                .filter(|item| item.is_string())
                .map(|item| normalized(item, 1000))
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    let previous = recent
        .iter()
        .map(|message| current_signals(message))
        .collect::<Vec<_>>();
    let prior_names = arguments["previousToolNames"]
        .as_array()
        .map(|names| {
            names[names.len().saturating_sub(32)..]
                .iter()
                .filter_map(Value::as_str)
                .filter(|name| matches(r"^[a-zA-Z0-9._-]{1,256}$", name))
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    let relation = discovery_relation(&text, &arguments["taskRelation"]);
    let followup = matches!(relation.as_str(), "continue" | "supplement")
        && arguments["taskRelation"]["allowsInheritance"] != false
        && projection["excludedClauses"].as_array().unwrap().is_empty();
    let is = |name: &str| signals[name] == true;
    let new_task = is("docs") && !is("desktop") && !is("browser") && !is("hostTerminal")
        || is("deviceState") && !is("desktop") && !is("browser")
        || matches!(
            device_state_topic(projection["activeText"].as_str().unwrap()),
            Some("external" | "explanation" | "non-device")
        );
    let mut retained = Vec::<String>::new();
    let mut boundary = browser_boundary(&text);
    if followup && !new_task {
        let mut subject = None;
        for index in (0..previous.len()).rev() {
            if [
                "desktop",
                "browser",
                "hostTerminal",
                "deviceState",
                "docs",
                "web",
            ]
            .iter()
            .any(|name| previous[index][*name] == true)
            {
                subject = Some(index);
                break;
            }
            if !matches!(
                discovery_relation(&recent[index], &Value::Null).as_str(),
                "continue" | "supplement"
            ) {
                break;
            }
        }
        if let Some(index) = subject {
            let subject = &previous[index];
            let device_followup = subject["deviceState"] == true
                && subject["browser"] != true
                && subject["desktop"] != true
                && signals["browser"] != true
                && signals["desktop"] != true;
            for key in ["desktop", "browser", "hostTerminal", "workspaceTerminal", "docs", "web"] {
                if subject[key] == true {
                    signals[key] = json!(true);
                }
            }
            if device_followup {
                signals["deviceState"] = json!(true);
            }
            if subject["browser"] == true && !device_followup && boundary.is_none() {
                boundary = browser_boundary(&recent[index]);
            }
            for name in prior_names {
                let category = category(&json!({"name":name}));
                let keep = category == "computer" && subject["desktop"] == true
                    || category == "host-terminal" && subject["hostTerminal"] == true
                    || category == "browser" && subject["browser"] == true
                    || category == "web-search" && subject["web"] == true
                    || category == "web-fetch"
                        && (subject["web"] == true || subject["url"] == true);
                if !keep {
                    continue;
                }
                if !retained.iter().any(|value| value == name) {
                    retained.push(name.to_owned());
                }
                match category {
                    "computer" => signals["desktop"] = json!(true),
                    "host-terminal" => {
                        signals["hostTerminal"] = json!(true);
                        if name.starts_with("terminal.workspace.") { signals["workspaceTerminal"] = json!(true); }
                    }
                    "browser" => signals["browser"] = json!(true),
                    _ => {}
                }
            }
        }
    }
    if let Some(boundary) = boundary {
        signals["remoteBrowser"] = json!(boundary == "remote");
        signals["browser"] = json!(true);
        signals["desktop"] = json!(boundary == "local");
    }
    if signals["remoteBrowser"] == true
        && !matches(
            r"本机桌面|本地窗口|\blocal desktop\b",
            projection["activeText"].as_str().unwrap(),
        )
    {
        signals["desktop"] = json!(false);
    }
    if signals["deviceState"] == true {
        signals["hostTerminal"] = json!(true);
    }
    signals["retainedNames"] = json!(retained);
    signals["taskRelation"] = json!(relation);
    signals["selectionOnly"] = json!(true);
    signals["semanticVerified"] = json!(false);
    signals
}

fn catalog_score(tool: &Value, signals: &Value) -> i32 {
    let name = tool["name"].as_str().unwrap_or("").to_lowercase();
    let description = tool["description"].as_str().unwrap_or("").to_lowercase();
    let yes = |key: &str| signals[key] == true;
    if name.starts_with("terminal.workspace.") {
        return if yes("workspaceTerminal") { 80 } else { 0 };
    }
    if name.starts_with("computer.") {
        if name == "computer.launch" && yes("browser") && signals["browserPresentation"] == "workspace" {
            return 0;
        }
        return if yes("desktop") { 40 } else { 0 };
    }
    if name.starts_with("terminal.host.") {
        return if !yes("hostTerminal") {
            0
        } else if name == "terminal.host.run" {
            if yes("deviceState") { 80 } else { 40 }
        } else {
            32
        };
    }
    let mut score = signals["terms"]
        .as_array()
        .map(|terms| {
            terms
                .iter()
                .filter_map(Value::as_str)
                .map(|word| {
                    if name.contains(word) {
                        3
                    } else if description.contains(word) {
                        1
                    } else {
                        0
                    }
                })
                .sum()
        })
        .unwrap_or(0);
    let docs = matches(
        r"context7|query[-_]docs|resolve[-_]library[-_]id|(?:search|fetch|get)[-_](?:docs|documentation)",
        &name,
    );
    let web_search = matches(
        r"web[-_]?search|search[-_]?web|search[-_]?news|news[-_]?search",
        &name,
    ) || matches(
        r"public web search|search (?:the )?(?:web|internet)",
        &description,
    );
    let search = matches(r"(?:^|[._-])search(?:$|[._-])", &name);
    let fetch = matches(
        r"(?:^|[._-])(?:fetch|web_fetch|fetch_url)(?:$|[._-])",
        &name,
    );
    if yes("browser") && category(tool) == "browser" {
        score += 32;
    }
    if name == "artifact.open" && (yes("browser") || yes("url"))
        && signals["browserPresentation"] == "workspace"
    {
        score += 48;
    }
    if signals["retainedNames"]
        .as_array()
        .is_some_and(|names| names.iter().any(|item| item == &tool["name"]))
    {
        score += 24;
    }
    if yes("docs") && docs {
        score += 28;
    }
    if yes("web") || yes("docs") {
        score += if web_search {
            if yes("docs") { 12 } else { 24 }
        } else if search {
            6
        } else {
            0
        };
    }
    if fetch && (yes("web") || yes("docs") || yes("url")) {
        score += if yes("url") { 32 } else { 10 };
    }
    score
}

fn catalog_order(arguments: &Value) -> Value {
    let descriptors = arguments["descriptors"]
        .as_array()
        .map(Vec::as_slice)
        .unwrap_or(&[]);
    let signals = &arguments["signals"];
    let scores = descriptors
        .iter()
        .map(|tool| catalog_score(tool, signals))
        .collect::<Vec<_>>();
    let flag = |condition: bool| i32::from(condition);
    let priority = |tool: &Value| {
        let name = tool["name"].as_str().unwrap_or("");
        [
            flag(["tool.search", "tool.load"].contains(&name)),
            flag(["tool.search", "tool.load", "tool.result.read"].contains(&name)),
            flag(signals["deviceState"] == true && name == "terminal.host.run"),
            flag(signals["hostTerminal"] == true && name.starts_with("terminal.host.")),
            flag(signals["remoteBrowser"] == true && category(tool) == "browser"),
            flag(signals["desktop"] == true && name.starts_with("computer.")),
            -flag(tool["modelExposure"] == "on-demand"),
            flag(
                tool["source"] == "builtin"
                    && tool["modelExposure"] != "on-demand"
                    && !["computer", "host-terminal"].contains(&category(tool)),
            ),
        ]
    };
    let mut indices = (0..descriptors.len()).collect::<Vec<_>>();
    indices.sort_by(|left, right| {
        priority(&descriptors[*right])
            .cmp(&priority(&descriptors[*left]))
            .then(scores[*right].cmp(&scores[*left]))
            .then(
                arguments["nameOrder"][*left]
                    .as_u64()
                    .cmp(&arguments["nameOrder"][*right].as_u64()),
            )
    });
    json!(indices)
}

pub(super) fn calculate(operation: &str, arguments: &Value) -> Result<Value, String> {
    Ok(match operation {
        "tools.discoveryCategory" => json!(category(&arguments["tool"])),
        "tools.searchTools" => search_tools(arguments),
        "tools.selectionSignals" => selection_signals(arguments),
        "tools.catalogOrder" => catalog_order(arguments),
        "tools.relevantMcpServers" => relevant_servers(arguments),
        _ => return Err("AGENT_CORE_UNKNOWN_OPERATION".to_owned()),
    })
}

fn relevant_servers(arguments: &Value) -> Value {
    let signals = &arguments["signals"];
    let mut selected = Vec::new();
    for server in arguments["servers"]
        .as_array()
        .map(Vec::as_slice)
        .unwrap_or(&[])
    {
        let id = server["id"].as_str().unwrap_or("");
        let label = format!("{} {}", id, server["name"].as_str().unwrap_or("undefined"))
            .nfkc()
            .collect::<String>()
            .to_lowercase();
        let components = pattern("[a-z0-9]+")
            .find_iter(&label)
            .map(|value| value.as_str().to_owned())
            .collect::<Vec<_>>();
        let retained = signals["retainedNames"].as_array().is_some_and(|names| {
            names
                .iter()
                .filter_map(Value::as_str)
                .any(|name| name.starts_with(&format!("mcp.{id}.")))
        });
        let selected_server = if retained {
            true
        } else if matches(r"playwright|chrome[-_.]?devtools|puppeteer|browser", &label) {
            arguments["explicitBrowser"] == true
        } else if signals["web"] == true && matches("exa|brave|search", &label) {
            true
        } else if signals["docs"] == true && matches("context7|deepwiki|documentation|docs", &label)
        {
            true
        } else {
            signals["terms"].as_array().is_some_and(|terms| {
                terms.iter().filter_map(Value::as_str).any(|term| {
                    if GENERIC_STARTUP_TERMS.contains(&term) {
                        return false;
                    }
                    if matches(r"\p{Han}", term) {
                        super::intent::utf16_length(term) >= 2 && label.contains(term)
                    } else {
                        super::intent::utf16_length(term) >= 3
                            && (components.iter().any(|value| value == term)
                                || term == id.to_lowercase())
                    }
                })
            })
        };
        if selected_server {
            selected.push(id.to_owned());
            if selected.len() == 4 {
                break;
            }
        }
    }
    json!(selected)
}

const GENERIC_STARTUP_TERMS: &[&str] = &[
    "the", "this", "that", "these", "those", "and", "for", "with", "from", "into", "about",
    "please", "check", "show", "read", "write", "find", "get", "use", "open", "run", "tool",
    "tools", "service", "services", "server", "servers", "local", "current", "now", "help", "工具",
    "服务", "使用", "检查", "查看", "打开", "读取", "运行", "当前", "本地", "这些", "这个", "那个",
];

fn has_device_subject(text: &str) -> bool {
    return matches(
        "网卡|适配器|代理(?:设置|配置)|dns(?:设置|配置|服务器)|进程|端口|\\b(?:ip(?:v[46])?|dns|proxy|adapters?|interfaces?|process(?:es)?|ports?|ipconfig|netstat|tasklist|get-process|get-nettcpconnection|get-netadapter|get-netipconfiguration|ping|tracert|traceroute|nslookup|resolve-dnsname|test-netconnection)\\b",
        text,
    ) || matches(
        "网络(?:配置|状态)|当前(?:的)?网络|现在(?:的)?网络|\\b(?:current network|network (?:status|configuration|adapters?))\\b",
        text,
    );
}

fn device_state_topic(text: &str) -> Option<&'static str> {
    let command = matches(
        "\\b(?:ipconfig|netstat|tasklist|get-process|get-nettcpconnection|get-netadapter|get-netipconfiguration|ping|tracert|traceroute|nslookup|resolve-dnsname|test-netconnection)\\b",
        text,
    );
    let topic = command
        || matches(
            "代理|网络|网卡|适配器|进程|端口|\\b(?:ip(?:v[46])?|dns|proxy|network|adapters?|interfaces?|process(?:es)?|ports?)\\b",
            text,
        );
    if !topic {
        return None;
    }
    // A local owner is insufficient for a relationship or neural-network subject.
    // “我的”不能把关系网络或神经网络变为设备诊断；真实连接与设备术语仍可提示候选。
    let non_device_network = matches(
        "人际|关系网络|社交网络|人脉|神经网络|\\b(?:social|professional|personal|neural|relationship)\\s+networks?\\b|\\bnetwork\\s+of\\s+(?:mentors|friends|contacts|colleagues|people)\\b",
        text,
    );
    let physical_network = matches(
        "网卡|网络适配器|网络(?:连接|连通|配置|状态)|(?:电脑|计算机|设备|本机|本地).{0,8}网络|\\b(?:network\\s+(?:connectivity|connections?|configuration|status|adapters?|interfaces?)|(?:computer|device|local|host)\\s+network)\\b",
        text,
    );
    if non_device_network
        && !command
        && !matches(
            "网卡|网络适配器|\\b(?:ip(?:v[46])?|dns|proxy|adapters?|interfaces?|ports?)\\b",
            text,
        )
    {
        return Some("non-device");
    }
    let local_owner = matches(
        "(?:我的|本机|本地|这台(?:电脑|计算机|机器|设备)|当前(?:电脑|设备|机器)).{0,24}(?:ip(?:v[46])?\\b|dns\\b|代理|网络|网卡|适配器|进程|端口)",
        text,
    ) || matches(
        "\\b(?:my|this (?:computer|machine|device|pc)|local(?:host)?|host)\\b.{0,40}\\b(?:ip(?:v[46])?|dns|proxy|network|adapters?|interfaces?|process(?:es)?|ports?)\\b",
        text,
    );
    let inspection = matches(
        "查(?:询|看|一下|查)?|检查|检测|看看|列出|显示|读取|获取|监控|\\b(?:check|inspect|list|show|read|detect|find|fetch|monitor)\\b",
        text,
    );
    let explanation = matches(
        "科普|原理|概念|教程|定义|区别|行业|趋势|(?:如何|怎样|怎么).{0,16}(?:查看|查询|配置|设置|检查|检测)|(?:解释|介绍).{0,20}(?:ip|dns|代理|网络|网卡|进程|端口|netstat|tasklist|ping|nslookup)",
        text,
    ) || matches(
        "(?:ip|dns|代理|网络|网卡|进程|端口|netstat|tasklist|ping|nslookup).{0,12}(?:是什么|什么意思|有什么作用|工作原理)",
        text,
    ) && !local_owner
        || matches(
            "\\b(?:explain|tutorial|concept|meaning|difference|trends?)\\b|\\bhow\\s+(?:to|does|do)\\b",
            text,
        )
        || matches(
            "\\bwhat\\s+is\\s+(?:an?\\s+|the\\s+)?(?:ip(?:v[46])?|dns|a proxy|a network|a process|a port)\\b",
            text,
        );
    let script_only = matches(
        "(?:写|生成|提供|给我).{0,12}(?:代码|脚本)|\\b(?:write|generate|provide)\\b.{0,20}\\b(?:code|script)\\b",
        text,
    ) && !matches(
        "(?:并|然后)(?:执行|运行)|\\brun\\s+(?:it|the script)\\b",
        text,
    );
    if script_only || explanation {
        return Some("explanation");
    }
    let public_lookup = matches(
        "归属|属地|运营商|所在地|地理位置|哪里的|是谁的|\\b(?:whois|geolocation|ownership)\\b|\\b(?:who owns|where is|ip location|lookup this ip)\\b",
        text,
    );
    let specified_ip = matches(
        "(?:^|[^0-9])(?:[0-9]{1,3}\\.){3}[0-9]{1,3}(?:$|[^0-9])",
        text,
    ) || matches("\\b(?:ipv6|ip address)\\b.{0,12}[a-f0-9]+:[a-f0-9:]+", text);
    if specified_ip && public_lookup || !local_owner && (public_lookup || specified_ip && !command)
    {
        return Some("external");
    }
    let local_status = matches(
        "当前(?:的)?(?:ip|网络|代理|dns|网卡|进程)|现在(?:的)?网络|当前(?:使用|运行).{0,12}(?:代理|dns|进程)|网络(?:配置|状态)|代理(?:配置|设置)|dns(?:配置|设置|服务器)|网卡|网络适配器|进程列表|运行中的进程|列出(?:所有|当前|本机|本地)?进程|端口占用|监听端口",
        text,
    ) || matches(
        "\\b(?:current (?:network|proxy|dns|ip|processes)|network (?:status|configuration)|proxy (?:settings|configuration)|dns (?:settings|servers?|configuration)|network adapters?|running processes|list processes|listening ports?)\\b",
        text,
    );
    let direct_status = matches(
        "^(?:(?:帮我|请)\\s*)?(?:查看|检查|检测|查询|查一下|看看|列出|显示|读取)\\s*(?:当前(?:的)?网络|代理(?:配置|设置)?|dns(?:配置|设置)?|网卡|网络适配器|本机进程|进程列表|端口占用)[呢吗?？!！。.\\s]*$",
        text,
    ) || matches(
        "^(?:当前(?:的)?(?:ip|网络)|网络(?:配置|状态)|代理(?:配置|设置)|dns(?:配置|设置)|进程列表|端口占用|current network|network status)[呢吗?？!！。.\\s]*$",
        text,
    ) || matches(
        "^(?:please\\s+)?(?:check|inspect|list|show|read)\\s+(?:(?:my|current|local)\\s+)?(?:proxy|dns|network|network adapters?|processes|listening ports?)[?.!\\s]*$",
        text,
    );
    let port_owner = matches(
        "(?:哪个|什么|谁).{0,12}(?:程序|进程|软件|应用)?.{0,12}(?:占用|占了|占|监听).{0,16}(?:端口|port)|端口.{0,12}(?:谁|哪个(?:程序|进程)).{0,12}(?:占用|占了|占|监听)",
        text,
    ) || matches(
        "\\b(?:which|what)\\s+(?:process|program|application|app)\\b.{0,32}\\b(?:port|listening)\\b|\\bwhat(?:'s| is) using (?:port|[0-9]{2,5})\\b",
        text,
    );
    if local_owner && (has_device_subject(text) || physical_network)
        || command
        || inspection && (local_status || physical_network)
        || direct_status && !non_device_network
        || port_owner
    {
        Some("device")
    } else {
        None
    }
}

fn current_signals(message: &str) -> Value {
    let projection = analyze_clauses(&normalized(&json!(message), 2000));
    let text = projection["activeText"].as_str().unwrap();
    let terms = query_terms(text);
    let file_references = file_references(text);
    let react_api = matches(
        "\\breact\\b.{0,48}\\b(?:hooks?|useeffect|usestate|usecontext|components?|jsx)\\b|\\b(?:hooks?|useeffect|usestate|usecontext|jsx)\\b.{0,48}\\breact\\b|\\b(?:useeffect|usestate|usecontext)\\s*\\(",
        text,
    );
    let browser = matches(
        "浏览器|网页|网站|网址|页面|chrome|firefox|playwright|devtools|\\bedge\\b|\\bbrowser\\b",
        text,
    );
    let remote_browser = matches(
        "(?:云端|远程).{0,16}浏览器|\\b(?:remote|cloud)\\s+browser\\b",
        text,
    );
    let device_topic = device_state_topic(text);
    let device_state = device_topic == Some("device")
        && (!remote_browser
            || matches(
                "本机|本地|我的电脑|\\blocal (?:computer|machine|device)\\b",
                text,
            ));
    let workspace_terminal = workspace_terminal_topic(text);
    let mut signals = json!({"terms":terms,"workspaceTerminal":workspace_terminal,
    "web": matches("最新|最近|当前|现在|今天|今日|新闻|官网|官方|上线|发布|搜索|查询|查证|联网|搜一下|是谁|什么时候|什么时间|多少钱", text) ||
      matches("\\b(?:latest|current|recent|today|news|official|released?|announced?|search|who|when|price|weather)\\b|\\blook\\s+up\\b", text),
    "docs": matches("文档|接口|代码|编程|开发|框架|库的|库怎么", text) ||
      matches("\\b(?:api|sdk|docs?|documentation|library|libraries|framework|programming|code|typescript|python|dotnet|winui)\\b", text) ||
      react_api || file_references.as_array().unwrap().iter().any(|reference| reference["domain"] == "code"),
    "desktop": matches("截图|截屏|屏幕|鼠标|键盘|光标|输入框|桌面|打开软件|打开应用|控制本机|浏览器|记事本|计算器|打开界面|可见窗口|调整窗口|窗口大小|最大化|最小化|恢复窗口", text) ||
      matches("(?:输入|键入|填写|填入|填进|粘贴).{0,24}(?:窗口|界面|页面|输入框)|\\b(?:type|enter|paste)\\b.{0,40}\\b(?:caret|focused field|text field|input field)\\b", text) ||
      matches("(?:打开|访问|浏览|查看|看看).{0,30}(?:网站|网页|网址|页面)", text) ||
      matches("(?:打开|启动|显示|使用|控制).{0,30}(?:chrome|edge|firefox|google)|\\b(?:open|launch|show|use|control)\\s+(?:the\\s+)?(?:local\\s+)?(?:chrome|edge|firefox|google)\\b", text) ||
      matches("(?:打开|访问|浏览|查看|看看|\\bopen\\b|\\bvisit\\b).{0,40}(?:[a-z0-9-]+\\.)+[a-z]{2,}", text) ||
      matches("(?:本机|本地).{0,30}(?:登录|界面|窗口|软件|应用)", text) ||
      matches("\\b(?:screenshot|desktop|mouse|browser|notepad|calculator|launch|computer)\\b", text),
    "browser":browser, "browserPresentation":super::intent::browser_presentation(text), "remoteBrowser":remote_browser, "deviceState":device_state,
    "hostTerminal": workspace_terminal || matches("本机终端|本地终端|本机命令|本地命令|宿主终端|conda|powershell|cmd|命令提示符", text) ||
      matches("后台.{0,12}(?:终端|进程|任务)|(?:终端|进程).{0,12}(?:后台|监控)|\\b(?:background|persistent)\\s+(?:terminal|process|job)\\b", text) ||
      matches("\\b(?:host|local|visible)\\b.*\\b(?:terminal|command)\\b|\\bterminal\\b.*\\b(?:window|visible)\\b", text) ||
      matches("(?:打开|显示|可见|可以看到|看得到).{0,20}终端|终端.{0,20}(?:窗口|可见|可以看到|看得到)", text),
    "url": matches("https?:\\/\\/\\S+", text), "fileReferences":file_references });
    if device_topic == Some("external") {
        signals["web"] = json!(true);
    }
    if device_state {
        signals["hostTerminal"] = json!(true);
        // Reading device state does not imply opening a GUI or performing an ordinary web search.
        // 读取设备状态不意味着打开界面或做普通网页搜索；用户明确要求的浏览器/界面动作仍保留。
        if !browser
            && !matches(
                "截图|截屏|屏幕|鼠标|键盘|光标|窗口|界面|打开|启动|\\b(?:screenshot|mouse|keyboard|window|launch|open)\\b",
                text,
            )
        {
            signals["desktop"] = json!(false);
        }
        if !browser
            && signals["url"] != true
            && !matches(
                "归属|属地|运营商|\\b(?:whois|geolocation|ownership)\\b",
                text,
            )
        {
            signals["web"] = json!(false);
        }
    }
    if remote_browser && !matches("本机桌面|本地窗口|\\blocal desktop\\b", text) {
        signals["desktop"] = json!(false);
    }
    signals
}

fn computer_alias(action: &str) -> &'static str {
    match action {
        "windows" => "windows window list 窗口 窗口列表 当前窗口",
        "apps" => {
            "apps applications programs chrome edge firefox 浏览器 软件 应用 程序 软件列表 本机浏览器"
        }
        "launch" => {
            "launch open start chrome edge firefox browser 打开 启动 软件 应用 浏览器 本机浏览器 本地浏览器 可见窗口 打开界面"
        }
        "window" => {
            "window resize maximize minimize restore 调整窗口 窗口大小 放大 缩小 最大化 最小化 恢复窗口"
        }
        "activate" => "activate focus foreground 激活 切换 前台 窗口",
        "read" => {
            "read inspect accessible UI text browser content 读取 查看 浏览器内容 网页内容 页面内容 窗口文本"
        }
        "screenshot" => "screenshot capture screen 截图 截屏 屏幕截图 窗口截图",
        "move" => "move mouse 鼠标 移动",
        "click" => "click mouse 点击 鼠标",
        "scroll" => "scroll wheel 滚动 鼠标",
        "drag" => "drag mouse 拖动 拖拽 鼠标",
        "type" => {
            "type input fill enter text paste caret focused field keyboard 输入 键入 填写 填入 填进 粘贴 光标 输入框 键盘 文本"
        }
        "key" => "key shortcut keyboard 按键 快捷键 键盘",
        _ => "",
    }
}

#[cfg(test)]
mod search_score_tests {
    use super::search_tools;
    use serde_json::{Value, json};

    #[test]
    fn local_image_analysis_discovers_pixels_instead_of_text_or_desktop_capture() {
        let catalog: Value = serde_json::from_str(include_str!("gateway-catalog.json")).unwrap();
        for query in ["读取本地 photo.png，分析图片构图", "Inspect local photo.webp pixels", "查看这张照片的颜色"] {
            let hits = search_tools(&json!({"query":query,"descriptors":catalog["tools"]}));
            assert_eq!(catalog["tools"][hits[0].as_u64().unwrap() as usize]["name"], "image.read", "{query}");
        }
    }

    #[test]
    fn document_authoring_prefers_real_generators_without_matching_ordinary_chat() {
        let catalog: Value = serde_json::from_str(include_str!("gateway-catalog.json")).unwrap();
        for query in ["生成一份报告.docx", "Create an Excel workbook", "制作 PPT 幻灯片"] {
            let hits = search_tools(&json!({"query":query,"descriptors":catalog["tools"]}));
            let index = hits[0].as_u64().unwrap() as usize;
            assert_eq!(catalog["tools"][index]["name"], "document.create", "{query}");
        }
        for query in ["你好", "hello"] {
            assert!(search_tools(&json!({"query":query,"descriptors":catalog["tools"]})).as_array().unwrap().is_empty());
        }
        assert!(super::authoring_topics("读取 example.docx").is_empty());
        assert!(super::authoring_topics("查看这个 API 的代码文档").is_empty());
        assert_eq!(super::authoring_topics("生成一个 SVG 流程图"), vec!["visual-artifacts"]);
    }

    #[test]
    fn scored_search_preserves_legacy_candidates_and_relevance() {
        let descriptors = json!([
            {"name":"mcp.indexneedle","description":"Look up records."},
            {"name":"indexneedle","description":"Look up records."},
            {"name":"disabled.indexneedle","available":false},
            {"name":"unrelated","description":"Other capability."}
        ]);
        let legacy = search_tools(&json!({"query":"indexneedle","descriptors":descriptors}));
        let scored = search_tools(&json!({"query":"indexneedle","descriptors":descriptors,"includeScores":true}));
        let hits = scored.as_array().unwrap();
        assert_eq!(legacy, json!([1, 0]));
        assert_eq!(hits.iter().map(|hit| hit["index"].clone()).collect::<Vec<Value>>(), *legacy.as_array().unwrap());
        assert!(hits[0]["score"].as_u64().unwrap() > hits[1]["score"].as_u64().unwrap());
    }

    #[test]
    fn greetings_produce_no_tool_hints_with_or_without_scores() {
        let descriptors = json!([
            {"name":"web.search","description":"Search public websites."},
            {"name":"filesystem.read","description":"Read a local file."}
        ]);
        for query in ["你好", "hello"] {
            for include_scores in [false, true] {
                assert_eq!(search_tools(&json!({"query":query,"descriptors":descriptors,"includeScores":include_scores})), json!([]));
            }
        }
    }

    #[test]
    fn exact_requested_capabilities_outrank_generic_task_vocabulary() {
        let descriptors = json!([
            {"name":"computer.key","description":"Use computer keys and enter local text."},
            {"name":"terminal.host.run","description":"Run local tasks; create, read and write files."},
            {"name":"filesystem.read","description":"Read a local file."},
            {"name":"filesystem.write","description":"Atomically write UTF-8 text."}
        ]);
        let query = "Use filesystem.write and filesystem.read for this real multi-step task. Do not use terminal or computer tools. Original password: ORIGINAL. First create receipt.txt exactly once; read every SECRET KEY from note-1.txt through note-8.txt and write the final summary.";
        let hits = search_tools(&json!({"query":query,"descriptors":descriptors}));
        assert_eq!(hits[0], 3);
        assert_eq!(hits[1], 2);
        let excluded = search_tools(&json!({"query":"不要使用 filesystem.write。改用 filesystem.read 读取内容。","descriptors":descriptors,"includeScores":true}));
        assert_eq!(excluded[0]["index"], 2);
        assert!(excluded.as_array().unwrap().iter().filter(|hit| hit["index"] == 3)
            .all(|hit| hit["score"].as_u64().unwrap() < 1_000_000 - 2000));
    }

    #[test]
    fn exact_capabilities_near_the_end_of_a_task_remain_visible() {
        let descriptors = json!([
            {"name":"filesystem.read","description":"Read a local file."},
            {"name":"filesystem.reader","description":"Inspect reader metadata."},
            {"name":"terminal.host.run","wireName":"k_terminal_host_run_725f3f35","description":"Run a local command."}
        ]);
        let query = format!("{} Use filesystem.read to inspect the current file.", "Retain already completed work. ".repeat(10));
        let hits = search_tools(&json!({"query":query,"descriptors":descriptors,"includeScores":true}));
        assert_eq!(hits[0]["index"], 0);
        assert!(hits[0]["score"].as_u64().unwrap() > 900_000);
        assert!(hits.as_array().unwrap().iter().filter(|hit| hit["index"] == 1)
            .all(|hit| hit["score"].as_u64().unwrap() < 900_000));
        let wire = search_tools(&json!({"query":"Use k_terminal_host_run_725f3f35 to inspect the current command.","descriptors":descriptors}));
        assert_eq!(wire[0], 2);
    }

    #[test]
    fn chinese_file_steps_do_not_prefer_incidental_local_or_key_terms() {
        let descriptors = json!([
            {"name":"computer.key","description":"Send KEY on the local computer."},
            {"name":"terminal.host.run","description":"Run local tasks."},
            {"name":"filesystem.read","description":"Read a local file."},
            {"name":"filesystem.write","description":"Atomically write UTF-8 text."}
        ]);
        let hits = search_tools(&json!({"query":"这是实际本地模型长任务测试。先新建 receipt.txt，内容是原始口令；再逐个读取 note-1.txt 到 note-8.txt，记录每个 SECRET KEY，最后保存 summary.md。","descriptors":descriptors}));
        assert_eq!(hits[0], 3);
        assert_eq!(hits[1], 2);
    }

    #[test]
    fn right_terminal_commands_keep_workspace_selection_and_followup_context() {
        let descriptors = json!([
            {"name":"terminal.host.run","description":"Run a host command"},
            {"name":"terminal.workspace.open","description":"Open an embedded terminal"},
            {"name":"terminal.workspace.write","description":"Send input to a terminal"}
        ]);
        let right = search_tools(&json!({"query":"在右侧终端运行 ipconfig 查看我的ip","descriptors":descriptors}));
        assert_ne!(right[0], 0);
        let host = search_tools(&json!({"query":"查看我的ip","descriptors":descriptors}));
        assert_eq!(host[0], 0);
        let followup = super::selection_signals(&json!({"message":"继续", "historySignals":["在右侧终端运行命令"],
            "previousToolNames":["terminal.workspace.write"]}));
        assert_eq!(followup["workspaceTerminal"], true);
        assert_eq!(followup["hostTerminal"], true);
    }
}

using System.Text.Json;
using KYNXA_Desktop.Services;
using KYNXA_Desktop.ViewModels;
using Windows.ApplicationModel.DataTransfer;

namespace TranscriptUiSmoke;

public partial class App
{
    // Exercise the shipped renderer in the native WebView; fixtures never contact a model or execute tools.
    // 在原生 WebView 中验证随包渲染器；测试不连接模型，也不执行工具。
    private async Task CheckDiagramsAsync()
    {
        string originalLanguage = UiText.Language;
        string originalPalette = AppearanceService.Current.Id;
        var originalSize = _window!.AppWindow.Size;
        try
        {
            UiText.Initialize("zh-CN");
            _transcript.ClearSelection();
            await CheckDiagramTypesAsync();
            await CheckDiagramStreamingAsync();
            await CheckDiagramSafetyAsync();
            await CheckDiagramInteractionsAsync();
            await CheckDiagramSelectionResizeAsync();
            await CheckDiagramNavigationAsync();
            await CheckDiagramReviewCasesAsync();
        }
        finally
        {
            await EvalAsync<bool>("(() => { window.kynxaDiagrams?.closeViewer(); getSelection().removeAllRanges(); return true; })()");
            UiText.Initialize(originalLanguage);
            AppearanceService.Apply(originalPalette);
            _window.AppWindow.Resize(originalSize);
            await Task.Delay(150);
        }
    }

    private static string MermaidFence(string source) => "```mermaid\n" + source + "\n```";

    // DOM updates are asynchronous even within one chat; every wait belongs to its exact message.
    // 同一聊天内的 DOM 更新也是异步的；等待条件必须绑定本次消息，避免误读上一条图表状态。
    private static string DiagramBlockExpression(ConversationMessageViewModel row) =>
        "document.querySelector(" + JsonSerializer.Serialize($"article[data-message-id=\"{row.Message.Id}\"] .mermaid-block") + ")";

    private async Task CheckDiagramTypesAsync()
    {
        var fixtures = new (string Type, string Source, string Label)[]
        {
            ("flowchart", "flowchart TD\n  A[\"开始 Start<br/>Data: 第二行 Line two\"] --> B{\"检查参数 Check input\"}\n  B -->|有效 Yes| C[\"完成 Finish\"]\n  B -->|无效 No| D[\"`**提示用户** Verify input\n重新输入足够长的中文标签并保留清楚的换行 Long descriptive Markdown label automatically wraps into several readable lines without clipping`\"]", "开始"),
            ("graph", "graph LR\n  UI[桌面 UI] --> Gateway[模型网关 Gateway]\n  Gateway --> Model[模型 Model]", "桌面"),
            ("sequenceDiagram", "sequenceDiagram\n  participant UI as 桌面 UI\n  participant Gateway as 网关 Gateway\n  participant Model as 模型 Model\n  UI->>Gateway: 发送消息 Send\n  Gateway->>Model: 请求回复\n  Model-->>UI: 完成 Reply\n  Note over UI,Model: 中文说明<br/>English note", "桌面"),
            ("stateDiagram-v2", "stateDiagram-v2\n  [*] --> Waiting\n  state \"等待 Waiting\" as Waiting\n  state \"运行 Running\" as Running\n  state \"完成 Done\" as Done\n  Waiting --> Running: 启动 Start\n  Running --> Done\n  Done --> [*]", "等待"),
            ("classDiagram", "classDiagram\n  class Gateway {\n    +String status\n    +sendMessage()\n  }\n  class ModelClient {\n    <<interface>>\n    +generate()\n  }\n  Gateway --> ModelClient : 调用 Call", "Gateway"),
            ("erDiagram", "erDiagram\n  USER ||--o{ MESSAGE : 发送\n  USER {\n    int id PK\n    string name\n  }\n  MESSAGE {\n    int id PK\n    int userId FK\n    string content\n  }", "USER"),
            ("gantt", "gantt\n  title 课设进度 Project schedule\n  dateFormat YYYY-MM-DD\n  section 界面 UI\n  设计 Design :done, design, 2026-10-01, 2d\n  实现 Implement :active, impl, after design, 3d\n  验证 Verify :after impl, 1d", "课设"),
            ("mindmap", "mindmap\n  root((任务拆解 Task))\n    界面 UI\n      配色 Theme\n      流程图 Diagram\n    验证 Tests\n      中文 Chinese\n      English", "任务拆解")
        };
        var chat = Guid.NewGuid();
        var rows = fixtures.Select(fixture => Message(chat, "assistant", fixture.Type + "\n\n" + MermaidFence(fixture.Source))).ToArray();
        _transcript.ShowConversation(chat, rows);
        await WaitAsync(string.Join(" && ", rows.Select(row => DiagramBlockExpression(row) + "?.dataset.diagramState === 'ready'")), "all seven requested Mermaid types and graph alias render using the bundled runtime");
        for (int index = 0; index < fixtures.Length; index++)
        {
            string messageId = JsonSerializer.Serialize(rows[index].Message.Id.ToString());
            string source = JsonSerializer.Serialize(fixtures[index].Source);
            string label = JsonSerializer.Serialize(fixtures[index].Label);
            Check(await EvalAsync<bool>($$"""
                (() => {
                  const article = document.querySelector(`article[data-message-id=${CSS.escape({{messageId}})}]`);
                  const block = article.querySelector('.mermaid-block'), svg = block.querySelector('.mermaid-viewport svg');
                  const viewBox = svg?.viewBox.baseVal;
                  return !!svg && viewBox.width > 0 && viewBox.height > 0 && svg.textContent.includes({{label}})
                    && block.querySelector('.mermaid-source code').textContent === {{source}}
                    && !block.querySelector('.mermaid-source-details').open
                    && svg.querySelectorAll('text,tspan').length > 0;
                })()
                """), fixtures[index].Type + " has real SVG text, a valid viewBox, Chinese/English labels and its original source");
        }
        Check(await EvalAsync<bool>($$"""
            (() => {
              const svg = {{DiagramBlockExpression(rows[0])}}.querySelector('.mermaid-viewport svg');
              const node = [...svg.querySelectorAll('.node')].find(element => element.textContent.includes('提示用户'));
              const spans = [...(node?.querySelectorAll('text tspan') || [])].filter(span => span.textContent.trim() && span.getBBox().width > 0);
              const lines = new Set(spans.map(span => span.getBBox().y.toFixed(1)));
              return !!node && node.textContent.includes('Verify input') && node.textContent.includes('clipping') && lines.size >= 3;
            })()
            """), "Mermaid Markdown strings render their real newline and automatically wrap a long bilingual label into SVG text lines");
        _metrics["diagramTypes"] = fixtures.Select(fixture => fixture.Type).ToArray();
        Check(await EvalAsync<bool>("""
            [...document.querySelectorAll('.mermaid-viewport svg')].every(svg =>
              !svg.querySelector('script,foreignObject,image,iframe,object,embed,a,animate,set')
              && [...svg.querySelectorAll('*')].every(element => [...element.attributes].every(attribute =>
                !/^on/i.test(attribute.name) && (!/(^|:)href$/i.test(attribute.name) || /^#[\w.-]+$/.test(attribute.value))))
              && [...svg.querySelectorAll('style')].every(style => !/@import|(?:https?:|data:|javascript:)/i.test(style.textContent)))
            """), "every accepted diagram SVG is inert and contains no executable elements, event attributes, external references or imported styles");
        Check(await EvalAsync<bool>("""
            (() => {
              const frame = document.querySelector('iframe.mermaid-renderer-frame');
              const policy = document.querySelector('meta[http-equiv="Content-Security-Policy"]').content;
              return !!frame && new URL(frame.src).origin === location.origin
                && new URL(frame.src).pathname.endsWith('/Transcript/diagram-renderer.html')
                && frame.sandbox.contains('allow-scripts') && !frame.sandbox.contains('allow-same-origin')
                && frame.contentDocument === null && policy.includes("connect-src 'none'")
                && policy.includes("object-src 'none'");
            })()
            """), "the actually loaded renderer is a local opaque sandbox while the transcript keeps connection and object loading blocked");
        await EvalAsync<bool>("(() => { scrollTo(0, 0); return true; })()");
        await CaptureViewportAsync(Path.Combine(Path.GetTempPath(), "kynxa-transcript-mermaid-types.png"));
    }

    private async Task CheckDiagramStreamingAsync()
    {
        var chat = Guid.NewGuid();
        const string source = "flowchart TD\n  A[中文 Start] --> B[完成 Finish]";
        var streaming = Message(chat, "assistant", "流式输出\n\n```mermaid\nflowchart TD\n  A[中文", "streaming");
        string streamingBlock = DiagramBlockExpression(streaming);
        _transcript.ShowConversation(chat, [streaming]);
        await WaitAsync(streamingBlock + "?.dataset.diagramState === 'waiting'", "an open streaming fence waits without invoking the parser");
        Check(await EvalAsync<bool>("!document.querySelector('.mermaid-block .mermaid-viewport svg') && document.querySelector('.mermaid-source-details').open && document.querySelector('.mermaid-source code').textContent.includes('A[中文')"),
            "incomplete streaming syntax keeps readable source without a parser-error diagram");
        streaming.Message.Content = "流式输出\n\n```mermaid\n" + source;
        streaming.Refresh();
        await WaitAsync(streamingBlock + "?.querySelector('.mermaid-source code')?.textContent.includes('完成 Finish') && " + streamingBlock + "?.dataset.diagramState === 'waiting'", "a complete grammar inside an open fence still waits for the closing fence");
        streaming.Message.Content += "\n```";
        streaming.Refresh();
        await WaitAsync(streamingBlock + "?.dataset.diagramState === 'ready'", "a closed Mermaid block renders before the surrounding message finishes streaming");
        streaming.Message.Status = "completed";
        streaming.Refresh();
        await WaitAsync("!document.querySelector('.streaming-dot') && " + streamingBlock + "?.dataset.diagramState === 'ready'", "completion retains a valid diagram");

        var truncated = Message(chat, "assistant", "```mermaid\nflowchart TD\n  A[未结束", "interrupted");
        _transcript.ShowConversation(chat, [truncated]);
        await WaitAsync(DiagramBlockExpression(truncated) + "?.dataset.diagramState === 'waiting'", "an interrupted open fence remains a source block");
        Check(await EvalAsync<bool>("document.querySelector('.mermaid-block').dataset.mermaidReady === 'false' && !document.querySelector('.mermaid-viewport svg') && document.querySelector('.mermaid-source-details').open"),
            "a terminal incomplete fence is never handed to Mermaid as a complete diagram");

        const string invalid = "flowchart TD\n  A[broken --> B";
        var failed = Message(chat, "assistant", MermaidFence(invalid));
        _transcript.ShowConversation(chat, [failed]);
        await WaitAsync(DiagramBlockExpression(failed) + "?.dataset.diagramState === 'error'", "invalid closed Mermaid syntax converges to a short fallback");
        Check(await EvalAsync<bool>($$"""
            document.querySelector('.mermaid-source code').textContent === {{JsonSerializer.Serialize(invalid)}}
            && document.querySelector('.mermaid-source-details').open
            && !document.querySelector('.mermaid-viewport svg')
            && document.querySelector('.mermaid-block').textContent.length < 400
            """), "a render failure preserves exact source and avoids a verbose parser stack or broken SVG");
    }

    private async Task CheckDiagramSafetyAsync()
    {
        var sources = new[]
        {
            "%%{init: {'securityLevel':'loose','flowchart':{'htmlLabels':true}}}%%\nflowchart TD\n  A[Test] --> B[Done]",
            "---\nconfig:\n  securityLevel: loose\n---\nflowchart TD\n  A[Test] --> B[Done]",
            "flowchart TD\n  A[Test] --> B[Done]\n  click A \"javascript:window.__mermaidUnsafe=1\"",
            "flowchart TD\n  A[Test] --> B[Done]\n  click A \"https://example.invalid/mermaid-external\"",
            "flowchart TD\n  A[\"<script>window.__mermaidUnsafe=1</script>\"] --> B[Done]",
            "flowchart TD\n  A[\"<img src='https://example.invalid/image' onerror='window.__mermaidUnsafe=1'>\"] --> B[Done]",
            "flowchart TD\n  A[\"<svg onload='window.__mermaidUnsafe=1'><a href='javascript:alert(1)'>bad</a></svg>\"] --> B[Done]",
            "flowchart TD\n  A@{ img: \"https://example.invalid/image.png\", label: \"Remote image\" }"
        };
        await EvalAsync<bool>("(() => { window.__mermaidUnsafe = 0; return true; })()");
        var chat = Guid.NewGuid();
        var rows = sources.Select(source => Message(chat, "assistant", MermaidFence(source))).ToArray();
        _transcript.ShowConversation(chat, rows);
        await WaitAsync(string.Join(" && ", rows.Select(row => DiagramBlockExpression(row) + "?.dataset.diagramState === 'error'")), "configuration injection, executable links, arbitrary HTML and external images are rejected with source fallback");
        Check(await EvalAsync<bool>("window.__mermaidUnsafe === 0 && !document.querySelector('.mermaid-block script, .mermaid-block img, .mermaid-block foreignObject, .mermaid-block iframe, .mermaid-block a[href]')"),
            "untrusted diagram source creates no script, image, arbitrary HTML, embedded frame or clickable link in message DOM");
        Check(await EvalAsync<bool>("[...document.querySelectorAll('.mermaid-block')].every(block => block.querySelector('.mermaid-source-details').open && !block.querySelector('.mermaid-viewport svg') && block.querySelector('.mermaid-source code').textContent.length > 0)"),
            "all rejected sources stay readable and copyable");
        Check(await EvalAsync<bool>("document.querySelectorAll('iframe.mermaid-renderer-frame').length === 1 && [...document.querySelectorAll('iframe')].every(frame => frame.sandbox.contains('allow-scripts') && !frame.sandbox.contains('allow-same-origin') && !frame.sandbox.contains('allow-top-navigation') && !frame.sandbox.contains('allow-popups'))"),
            "the isolated renderer never receives same-origin, top-navigation or popup permissions");

        var large = Message(chat, "assistant", MermaidFence("flowchart TD\n  A[" + new string('x', 51000) + "] --> B[Done]"));
        _transcript.ShowConversation(chat, [large]);
        await WaitAsync(DiagramBlockExpression(large) + "?.dataset.diagramState === 'error'", "oversized diagram source has a bounded source-only fallback");
        Check(await EvalAsync<bool>("document.querySelector('.mermaid-source code').textContent.length > 51000 && !document.querySelector('.mermaid-viewport svg')"),
            "the renderer limit does not truncate original diagram source");
    }

    private async Task CheckDiagramInteractionsAsync()
    {
        const string source = "flowchart LR\n  A[\"中文 Start<br/>English line\"] --> B[\"验证输入和完整返回状态 Long descriptive label\"]\n  B --> C[\"完成 Done\"]";
        var chat = Guid.NewGuid();
        var row = Message(chat, "assistant", "DIAGRAM_BEFORE 前文\n\n" + MermaidFence(source) + "\n\nDIAGRAM_AFTER 后文");
        _transcript.ShowConversation(chat, [row]);
        await WaitAsync(DiagramBlockExpression(row) + "?.dataset.diagramState === 'ready'", "interactive diagram fixture renders");
        await EvalAsync<bool>("(() => { window.__diagramBlock = document.querySelector('.mermaid-block'); window.__diagramArticle = document.querySelector('article'); return true; })()");

        foreach (int width in new[] { 980, 480 })
        {
            _window!.AppWindow.Resize(new Windows.Graphics.SizeInt32(width, 850));
            await Task.Delay(220);
            await EvalAsync<bool>("(() => { scrollTo(0, 0); __diagramBlock.querySelector('[data-diagram-action=fit]').click(); return true; })()");
            await Task.Delay(120);
            var geometry = await EvalAsync<JsonElement>("(() => { const block=__diagramBlock.getBoundingClientRect(), view=__diagramBlock.querySelector('.mermaid-viewport').getBoundingClientRect(), svg=__diagramBlock.querySelector('.mermaid-viewport svg').getBoundingClientRect(); return {viewport:innerWidth,page:document.documentElement.scrollWidth,blockWidth:block.width,viewWidth:view.width,svgWidth:svg.width,svgLeft:svg.left,svgRight:svg.right,viewLeft:view.left,viewRight:view.right,buttons:[...__diagramBlock.querySelectorAll('button')].map(button=>({label:button.getAttribute('aria-label'),width:button.getBoundingClientRect().width}))}; })()");
            Check(geometry.GetProperty("page").GetDouble() <= geometry.GetProperty("viewport").GetDouble() + 1 && geometry.GetProperty("viewWidth").GetDouble() > 200,
                "diagram controls and long labels do not widen the native page at " + width);
            Check(geometry.GetProperty("svgLeft").GetDouble() >= geometry.GetProperty("viewLeft").GetDouble() - 2 && geometry.GetProperty("svgRight").GetDouble() <= geometry.GetProperty("viewRight").GetDouble() + 2,
                "fit keeps the complete diagram within its viewport at " + width);
            Check(geometry.GetProperty("buttons").EnumerateArray().All(button => !string.IsNullOrWhiteSpace(button.GetProperty("label").GetString()) && button.GetProperty("width").GetDouble() >= 24),
                "diagram controls retain accessible names and usable hit targets at " + width);
            _metrics["diagramGeometry" + width] = geometry;
            await CaptureViewportAsync(Path.Combine(Path.GetTempPath(), "kynxa-transcript-mermaid-" + width + ".png"));
        }
        _window!.AppWindow.Resize(new Windows.Graphics.SizeInt32(980, 850));
        await Task.Delay(180);
        await CheckDiagramCopyAsync(source);
        await CheckDiagramZoomAndViewerAsync();

        string partialLabel = await EvalAsync<string>("""
            (() => {
              const label = __transcriptSmoke.textNode(__diagramBlock.querySelector('.mermaid-viewport svg'), 'Start');
              const start = label.nodeValue.indexOf('Start') + 1;
              getSelection().removeAllRanges(); getSelection().setBaseAndExtent(label, start, label, start + 3);
              return __transcriptSmoke.copyEvent();
            })()
            """);
        Check(partialLabel == "tar", "partial SVG label selection copies only the selected visible letters instead of the full Mermaid source");
        string partialDiagram = await EvalAsync<string>("""
            (() => {
              const body = __diagramArticle.querySelector('.message-body');
              const first = __transcriptSmoke.textNode(body, 'DIAGRAM_BEFORE');
              const label = __transcriptSmoke.textNode(__diagramBlock.querySelector('.mermaid-viewport svg'), 'Start');
              getSelection().removeAllRanges();
              getSelection().setBaseAndExtent(first, 0, label, label.nodeValue.indexOf('Start') + 3);
              return __transcriptSmoke.copyEvent();
            })()
            """);
        _metrics["diagramPartialSelection"] = partialDiagram;
        Check(partialDiagram.StartsWith("DIAGRAM_BEFORE 前文", StringComparison.Ordinal)
            && partialDiagram.EndsWith("中文 Sta", StringComparison.Ordinal)
            && !partialDiagram.Contains("diagram-safe-", StringComparison.Ordinal)
            && !partialDiagram.Contains("font-family", StringComparison.Ordinal)
            && !partialDiagram.Contains("fill:", StringComparison.Ordinal),
            "selection from prose into part of a diagram copies visible text without generated SVG styles or definitions");
        string acrossDiagram = await EvalAsync<string>("""
            (() => {
              const body = __diagramArticle.querySelector('.message-body');
              const first = __transcriptSmoke.textNode(body, 'DIAGRAM_BEFORE');
              const last = __transcriptSmoke.textNode(body, 'DIAGRAM_AFTER');
              getSelection().removeAllRanges(); getSelection().setBaseAndExtent(first, 0, last, last.nodeValue.length);
              return __transcriptSmoke.copyEvent();
            })()
            """);
        Check(acrossDiagram.StartsWith("DIAGRAM_BEFORE 前文", StringComparison.Ordinal) && acrossDiagram.EndsWith("DIAGRAM_AFTER 后文", StringComparison.Ordinal)
            && acrossDiagram.Split(source, StringSplitOptions.None).Length == 2 && acrossDiagram.Split("中文 Start", StringSplitOptions.None).Length == 2
            && !acrossDiagram.Contains("复制源码", StringComparison.Ordinal) && !acrossDiagram.Contains("Mermaid ·", StringComparison.Ordinal),
            "selection across surrounding prose and a full diagram preserves source once and excludes duplicate SVG labels or toolbar text");
        string selected = await EvalAsync<string>("(() => { const range=document.createRange(); range.selectNode(__diagramBlock); getSelection().removeAllRanges(); getSelection().addRange(range); return __transcriptSmoke.copyEvent(); })()");
        Check(selected == source, "selecting a complete diagram copies its original Mermaid source once without SVG labels or toolbar text");
        _transcript.ClearSelection();
        await EvalAsync<bool>("(() => { const node=__transcriptSmoke.textNode(document.querySelector('.message-body'),'DIAGRAM_BEFORE'); getSelection().setBaseAndExtent(node,0,node,14); return true; })()");
        string savedSelection = await EvalAsync<string>("window.transcriptSelectionText()");
        double top = await EvalAsync<double>("scrollY");
        foreach (var palette in AppearanceService.Palettes)
        {
            AppearanceService.Apply(palette.Id);
            await WaitAsync($$"""
                getComputedStyle(document.documentElement).getPropertyValue('--appearance-accent').trim().toLowerCase() === '{{palette.Accent.ToLowerInvariant()}}'
                """, "the native appearance change reaches the transcript for " + palette.Id);
            Check(await EvalAsync<bool>($$"""
                document.querySelector('article') === __diagramArticle && document.querySelector('.mermaid-block') === __diagramBlock
                && getComputedStyle(document.documentElement).getPropertyValue('--appearance-accent').trim().toLowerCase() === '{{palette.Accent.ToLowerInvariant()}}'
                && window.transcriptSelectionText() === {{JsonSerializer.Serialize(savedSelection)}} && Math.abs(scrollY - {{top.ToString(System.Globalization.CultureInfo.InvariantCulture)}}) < 2
                """), "palette " + palette.Id + " updates diagram styling without replacing message DOM, selection or scroll");
            // A selected transcript defers SVG replacement; validate the new colors after clearing its range.
            // 有选区时延迟替换 SVG；清除选区后再检查真实新颜色，避免把旧 ready 状态误当作重绘完成。
            _transcript.ClearSelection();
            var soft = AppearanceService.ParseColor(palette.Soft);
            string expectedFill = $"rgb({soft.R}, {soft.G}, {soft.B})";
            await WaitAsync($$"""
                __diagramBlock.dataset.diagramState === 'ready' && !!__diagramBlock.querySelector('.mermaid-viewport svg .node rect')
                && getComputedStyle(__diagramBlock.querySelector('.mermaid-viewport svg .node rect')).fill === {{JsonSerializer.Serialize(expectedFill)}}
                """, "the actual SVG node fill matches palette " + palette.Id + " after the deferred repaint");
            await EvalAsync<bool>("(() => { const node=__transcriptSmoke.textNode(document.querySelector('.message-body'),'DIAGRAM_BEFORE'); getSelection().setBaseAndExtent(node,0,node,14); return true; })()");
        }
        UiText.Initialize("en");
        await WaitAsync("document.documentElement.lang === 'en' && __diagramBlock.querySelector('.mermaid-copy-source').getAttribute('aria-label').includes('source')", "existing diagram controls localize live to English");
        Check(await EvalAsync<bool>($$"""
            __diagramBlock.querySelector('.mermaid-source code').textContent === {{JsonSerializer.Serialize(source)}}
            && __diagramBlock.querySelector('.mermaid-viewport svg').textContent.includes('中文')
            && window.transcriptSelectionText() === {{JsonSerializer.Serialize(savedSelection)}}
            """), "language changes preserve model labels, original source and active selection");
        _transcript.ClearSelection();
    }

    private async Task CheckDiagramCopyAsync(string expectedSource)
    {
        {
            Check(await EvalAsync<bool>("(() => { const button=__diagramBlock.querySelector('.mermaid-copy-source'); window.__diagramCopy=button; button.click(); return button.dataset.copyState !== 'success'; })()"),
                "diagram copy does not announce success before the native clipboard acknowledgement");
            await WaitAsync("__diagramCopy.dataset.copyState === 'success' && !!__diagramCopy.querySelector('svg path')", "native copy acknowledgement changes the diagram copy icon to a checkmark");
            Check(await EvalAsync<bool>("__diagramBlock.querySelector('.mermaid-copy-status').textContent === __diagramCopy.getAttribute('aria-label') && !__diagramCopy.hasAttribute('title')"),
                "diagram copy displays its result beside the button without a floating title");
            Check(await Clipboard.GetContent().GetTextAsync() == expectedSource, "diagram source copy preserves exact Unicode, line breaks, indentation and HTML label syntax");
            await WaitAsync("!__diagramCopy.dataset.copyState && !!__diagramCopy.querySelector('svg rect')", "diagram copy feedback restores its normal icon");
        }
    }

    private async Task CheckDiagramZoomAndViewerAsync()
    {
        double fit = await EvalAsync<double>("Number(__diagramBlock.querySelector('.mermaid-viewport').dataset.scale)");
        await EvalAsync<bool>("(() => { __diagramBlock.querySelector('[data-diagram-action=zoom-in]').click(); return true; })()");
        Check(await EvalAsync<double>("Number(__diagramBlock.querySelector('.mermaid-viewport').dataset.scale)") > fit, "zoom in increases the SVG canvas scale");
        await EvalAsync<bool>("(() => { __diagramBlock.querySelector('[data-diagram-action=zoom-out]').click(); return true; })()");
        Check(Math.Abs(await EvalAsync<double>("Number(__diagramBlock.querySelector('.mermaid-viewport').dataset.scale)") - fit) < 0.02, "zoom out reverses the previous zoom step");
        await EvalAsync<bool>("(() => { __diagramBlock.querySelector('[data-diagram-action=zoom-in]').click(); __diagramBlock.querySelector('[data-diagram-action=zoom-in]').click(); return true; })()");
        string before = await EvalAsync<string>("__diagramBlock.querySelector('.mermaid-canvas').style.transform");
        var point = await EvalAsync<JsonElement>("(() => { const r=__diagramBlock.querySelector('.mermaid-viewport').getBoundingClientRect(); return {x:r.left+8,y:r.top+8}; })()");
        double x = point.GetProperty("x").GetDouble(), y = point.GetProperty("y").GetDouble();
        await DispatchMouse("mouseMoved", x, y);
        await DispatchMouse("mousePressed", x, y, "left", 1);
        await DispatchMouse("mouseMoved", x + 60, y + 25, "left", 1);
        await DispatchMouse("mouseReleased", x + 60, y + 25, "left");
        Check(await EvalAsync<string>("__diagramBlock.querySelector('.mermaid-canvas').style.transform") != before, "trusted WebView pointer dragging pans the diagram canvas");
        await EvalAsync<bool>("(() => { __diagramBlock.querySelector('[data-diagram-action=fit]').click(); __diagramBlock.querySelector('[data-diagram-action=fullscreen]').click(); return true; })()");
        await WaitAsync("document.querySelector('dialog.mermaid-viewer')?.open === true", "fullscreen action opens a modal viewer");
        Check(await EvalAsync<bool>("!!document.querySelector('.mermaid-viewer-viewport svg') && document.querySelector('dialog.mermaid-viewer').getBoundingClientRect().width > innerWidth * 0.8"),
            "fullscreen viewer presents an independent SVG across the available width");
        await CaptureViewportAsync(Path.Combine(Path.GetTempPath(), "kynxa-transcript-mermaid-fullscreen.png"));
        await _transcript.Browser.CoreWebView2.CallDevToolsProtocolMethodAsync("Input.dispatchKeyEvent", JsonSerializer.Serialize(new { type = "keyDown", key = "Escape", code = "Escape", windowsVirtualKeyCode = 27, nativeVirtualKeyCode = 27 }));
        await _transcript.Browser.CoreWebView2.CallDevToolsProtocolMethodAsync("Input.dispatchKeyEvent", JsonSerializer.Serialize(new { type = "keyUp", key = "Escape", code = "Escape", windowsVirtualKeyCode = 27, nativeVirtualKeyCode = 27 }));
        await WaitAsync("!document.querySelector('dialog.mermaid-viewer')?.open", "Escape closes the diagram viewer");
        Check(await EvalAsync<bool>("document.activeElement === __diagramBlock.querySelector('[data-diagram-action=fullscreen]')"), "closing the modal restores focus to its opener");
    }

    private async Task CheckDiagramSelectionResizeAsync()
    {
        _transcript.ClearSelection();
        UiText.Initialize("zh-CN");
        _window!.AppWindow.Resize(new Windows.Graphics.SizeInt32(980, 850));
        await Task.Delay(180);
        const string source = "flowchart TD\n  A[开始 Start] --> B[\"资料收集与输入校验<br/>Collect information and validate inputs\"]\n  A --> C[\"制定计划并拆分任务<br/>Make a plan and organize the work\"]\n  A --> D[\"确认权限和可用工具<br/>Check permissions and available tools\"]\n  B --> E[合并结果并完成回复 Finish]\n  C --> E\n  D --> E";
        var chat = Guid.NewGuid();
        var row = Message(chat, "assistant", "RESIZE_SELECTION 前文保持选中\n\n" + MermaidFence(source));
        _transcript.ShowConversation(chat, [row]);
        await WaitAsync(DiagramBlockExpression(row) + "?.dataset.diagramState === 'ready'", "a branching diagram establishes a naturally sized wide viewport");
        await EvalAsync<bool>("""
            (() => {
              const block = document.querySelector('.mermaid-block'), viewport = block.querySelector('.mermaid-viewport');
              const canvas = block.querySelector('.mermaid-canvas');
              const node = __transcriptSmoke.textNode(document.querySelector('.message-body'), 'RESIZE_SELECTION');
              getSelection().removeAllRanges(); getSelection().setBaseAndExtent(node, 0, node, 16);
              window.__diagramResize = { block, viewport, canvas, svg: viewport.querySelector('svg'),
                width: viewport.clientWidth, height: viewport.clientHeight, scale: Number(viewport.dataset.scale),
                inlineHeight: viewport.style.height, transform: canvas.style.transform, selected: window.transcriptSelectionText() };
              return true;
            })()
            """);
        _window.AppWindow.Resize(new Windows.Graphics.SizeInt32(480, 850));
        await WaitAsync("innerWidth < 500 && __diagramResize.viewport.clientWidth < __diagramResize.width - 200", "the actual native resize reaches the selected transcript");
        await Task.Delay(180);
        Check(await EvalAsync<bool>("""
            window.transcriptSelectionText() === __diagramResize.selected && !getSelection().isCollapsed
            && __diagramResize.viewport.style.height === __diagramResize.inlineHeight
            && __diagramResize.viewport.clientHeight === __diagramResize.height
            && __diagramResize.canvas.style.transform === __diagramResize.transform
            && __diagramResize.viewport.querySelector('svg') === __diagramResize.svg
            """), "resizing while prose is selected preserves the range and defers diagram height and transform changes");
        _transcript.ClearSelection();
        await WaitAsync("""
            getSelection().isCollapsed && __diagramResize.viewport.clientHeight < __diagramResize.height - 10
            && Number(__diagramResize.viewport.dataset.scale) < __diagramResize.scale
            """, "clearing selection automatically applies the pending natural height and fits the resized diagram");
        var geometry = await EvalAsync<JsonElement>("""
            (() => {
              const view = __diagramResize.viewport.getBoundingClientRect(), svg = __diagramResize.svg.getBoundingClientRect();
              return { wideWidth: __diagramResize.width, wideHeight: __diagramResize.height,
                narrowWidth: view.width, narrowHeight: view.height, wideScale: __diagramResize.scale,
                narrowScale: Number(__diagramResize.viewport.dataset.scale), left: svg.left, right: svg.right,
                top: svg.top, bottom: svg.bottom, viewLeft: view.left, viewRight: view.right,
                viewTop: view.top, viewBottom: view.bottom, page: document.documentElement.scrollWidth, viewport: innerWidth };
            })()
            """);
        Check(geometry.GetProperty("left").GetDouble() >= geometry.GetProperty("viewLeft").GetDouble() - 2
            && geometry.GetProperty("right").GetDouble() <= geometry.GetProperty("viewRight").GetDouble() + 2
            && geometry.GetProperty("top").GetDouble() >= geometry.GetProperty("viewTop").GetDouble() - 2
            && geometry.GetProperty("bottom").GetDouble() <= geometry.GetProperty("viewBottom").GetDouble() + 2
            && geometry.GetProperty("page").GetDouble() <= geometry.GetProperty("viewport").GetDouble() + 1,
            "the deferred resize keeps the complete SVG reachable without clipping or horizontal page overflow");
        _metrics["diagramDeferredResize"] = geometry;
        _window.AppWindow.Resize(new Windows.Graphics.SizeInt32(980, 850));
        await Task.Delay(180);
    }

    private async Task CheckDiagramNavigationAsync()
    {
        UiText.Initialize("zh-CN");
        var firstChat = Guid.NewGuid();
        var first = Message(firstChat, "assistant", MermaidFence("flowchart TD\n  A[FIRST_DIAGRAM] --> B[完成]"));
        _transcript.ShowConversation(firstChat, [first]);
        await WaitAsync(DiagramBlockExpression(first) + "?.dataset.diagramState === 'ready' && " + DiagramBlockExpression(first) + "?.querySelector('.mermaid-viewport svg').textContent.includes('FIRST_DIAGRAM')", "the first conversation caches a rendered diagram");
        await EvalAsync<bool>("(() => { window.__firstDiagramArticle=document.querySelector('article'); return true; })()");
        var secondChat = Guid.NewGuid();
        var second = Message(secondChat, "assistant", "SECOND_DIAGRAM_CHAT 原文");
        _transcript.ShowConversation(secondChat, [second]);
        await WaitAsync($"window.transcriptState().conversationId === '{secondChat}' && __transcriptSmoke.bodyText().includes('SECOND_DIAGRAM_CHAT')", "switching away shows only the second conversation");
        Check(await EvalAsync<bool>("!document.querySelector('.mermaid-block') && !document.querySelector('dialog.mermaid-viewer')?.open"), "cached diagrams and modal viewers do not leak into another conversation");
        UiText.Initialize("en");
        await WaitAsync("document.documentElement.lang === 'en'", "a language change reaches the conversation while the first diagram is cached");
        _transcript.ShowConversation(firstChat, [first]);
        await WaitAsync("document.querySelector('article') === __firstDiagramArticle && document.querySelector('.mermaid-block')?.dataset.diagramState === 'ready'", "cached reopening reuses the original message and rendered diagram");
        Check(await EvalAsync<bool>("""
            (() => {
              const block = document.querySelector('.mermaid-block');
              return block.querySelector('.mermaid-copy-source').getAttribute('aria-label').toLowerCase().includes('source')
                && block.querySelector('[data-diagram-action=zoom-in]').getAttribute('aria-label').toLowerCase().includes('zoom')
                && block.querySelector('.mermaid-source-details summary').textContent.toLowerCase().includes('source')
                && block.querySelector('.mermaid-viewport').getAttribute('aria-label').toLowerCase().includes('diagram');
            })()
            """), "cached diagram controls adopt a language selected in another conversation");

        var pendingChat = Guid.NewGuid();
        string large = "flowchart TD\n" + string.Join("\n", Enumerable.Range(0, 220).Select(index => $"  A{index}[PENDING_{index}] --> A{index + 1}"));
        var pending = Message(pendingChat, "assistant", MermaidFence(large));
        _transcript.ShowConversation(pendingChat, [pending]);
        await WaitAsync(DiagramBlockExpression(pending) + " != null", "an asynchronous larger diagram enters the presentation path");
        _transcript.ShowConversation(secondChat, [second]);
        await WaitAsync($"window.transcriptState().conversationId === '{secondChat}' && __transcriptSmoke.bodyText().includes('SECOND_DIAGRAM_CHAT') && !document.querySelector('.mermaid-block')", "rapid navigation abandons the old active diagram");
        await Task.Delay(500);
        Check(await EvalAsync<bool>("__transcriptSmoke.bodyText().includes('SECOND_DIAGRAM_CHAT') && !document.querySelector('.mermaid-block') && !document.querySelector('.mermaid-viewer')?.open"),
            "late asynchronous diagram results cannot replace the current conversation or reopen a viewer");
        _metrics["diagramNavigation"] = "cached DOM reused; late old render isolated from active conversation";
    }

    // Cover cross-feature review cases that isolated rendering and inline selection checks cannot exercise.
    // 验证单独渲染和行内选区检查无法覆盖的跨功能审查场景。
    private async Task CheckDiagramReviewCasesAsync()
    {
        UiText.Initialize("zh-CN");
        const string comparisonSource = "flowchart TD\n  A{\"x < y?\"} --> B[Yes]";
        var comparisonChat = Guid.NewGuid();
        var comparison = Message(comparisonChat, "assistant", MermaidFence(comparisonSource));
        _transcript.ShowConversation(comparisonChat, [comparison]);
        await WaitAsync(DiagramBlockExpression(comparison) + "?.dataset.diagramState === 'ready' || "
            + DiagramBlockExpression(comparison) + "?.dataset.diagramState === 'error'", "a plain mathematical comparison reaches a terminal diagram state");
        Check(await EvalAsync<bool>(DiagramBlockExpression(comparison) + "?.dataset.diagramState === 'ready' && "
            + DiagramBlockExpression(comparison) + "?.querySelector('.mermaid-viewport svg').textContent.includes('x < y?')"),
            "ordinary comparison text is preserved as a label without being mistaken for executable HTML");

        const string ordinaryMindmapSource = "mindmap\n  root((Interaction notes))\n    click events\n    links and references";
        var ordinaryMindmap = Message(comparisonChat, "assistant", MermaidFence(ordinaryMindmapSource));
        _transcript.ShowConversation(comparisonChat, [ordinaryMindmap]);
        await WaitAsync(DiagramBlockExpression(ordinaryMindmap) + "?.dataset.diagramState === 'ready' || "
            + DiagramBlockExpression(ordinaryMindmap) + "?.dataset.diagramState === 'error'", "ordinary interaction words reach a terminal mindmap state");
        Check(await EvalAsync<bool>(DiagramBlockExpression(ordinaryMindmap) + "?.dataset.diagramState === 'ready' && "
            + DiagramBlockExpression(ordinaryMindmap) + "?.querySelector('.mermaid-viewport svg').textContent.includes('click events') && "
            + DiagramBlockExpression(ordinaryMindmap) + "?.querySelector('.mermaid-viewport svg').textContent.includes('links and references')"),
            "mindmap branch labels may discuss click events and links without being mistaken for diagram actions");

        const string ordinaryNodeSource = "flowchart LR\n  links --> references";
        var ordinaryNodes = Message(comparisonChat, "assistant", MermaidFence(ordinaryNodeSource));
        _transcript.ShowConversation(comparisonChat, [ordinaryNodes]);
        await WaitAsync(DiagramBlockExpression(ordinaryNodes) + "?.dataset.diagramState === 'ready' || "
            + DiagramBlockExpression(ordinaryNodes) + "?.dataset.diagramState === 'error'", "ordinary flowchart node names reach a terminal diagram state");
        Check(await EvalAsync<bool>(DiagramBlockExpression(ordinaryNodes) + "?.dataset.diagramState === 'ready' && "
            + DiagramBlockExpression(ordinaryNodes) + "?.querySelector('.mermaid-viewport svg').textContent.includes('references')"),
            "ordinary flowchart identifiers may be named links and references without invoking a link action");

        var viewerChat = Guid.NewGuid();
        var viewerRow = Message(viewerChat, "assistant", MermaidFence("flowchart LR\n  A[VIEWER_SELECTED] --> B[VIEWER_DONE]"));
        _transcript.ShowConversation(viewerChat, [viewerRow]);
        await WaitAsync(DiagramBlockExpression(viewerRow) + "?.dataset.diagramState === 'ready'", "the fullscreen selection fixture renders");
        await EvalAsync<bool>("(() => { document.querySelector('.mermaid-block [data-diagram-action=fullscreen]').click(); return true; })()");
        await WaitAsync("document.querySelector('dialog.mermaid-viewer')?.open === true", "the fullscreen selection fixture opens its viewer");
        await EvalAsync<bool>("""
            (() => {
              const viewport = document.querySelector('.mermaid-viewer-viewport'), svg = viewport.querySelector('svg');
              const label = __transcriptSmoke.textNode(svg, 'VIEWER_SELECTED');
              getSelection().removeAllRanges(); getSelection().setBaseAndExtent(label, 0, label, 6);
              window.__diagramViewerSelection = { viewport, svg, selected: getSelection().toString() };
              return true;
            })()
            """);
        var nextPalette = AppearanceService.Palettes.First(palette => palette.Id != AppearanceService.Current.Id);
        AppearanceService.Apply(nextPalette.Id);
        await WaitAsync($$"""
            getComputedStyle(document.documentElement).getPropertyValue('--appearance-accent').trim().toLowerCase() === '{{nextPalette.Accent.ToLowerInvariant()}}'
            """, "a native theme change reaches the fullscreen diagram while its text is selected");
        await Task.Delay(300);
        Check(await EvalAsync<bool>("""
            !getSelection().isCollapsed && getSelection().toString() === __diagramViewerSelection.selected
            && __diagramViewerSelection.viewport.querySelector('svg') === __diagramViewerSelection.svg
            """), "changing the palette preserves fullscreen selected text and defers replacement of its SVG");
        _transcript.ClearSelection();
        var soft = AppearanceService.ParseColor(nextPalette.Soft);
        string expectedFill = $"rgb({soft.R}, {soft.G}, {soft.B})";
        await WaitAsync($$"""
            getSelection().isCollapsed && __diagramViewerSelection.viewport.querySelector('svg') !== __diagramViewerSelection.svg
            && getComputedStyle(__diagramViewerSelection.viewport.querySelector('svg .node rect')).fill === {{JsonSerializer.Serialize(expectedFill)}}
            """, "clearing the fullscreen selection applies the latest theme to the existing open viewer");
        var completeViewerCopy = await EvalAsync<JsonElement>("""
            (() => {
              const svg = document.querySelector('.mermaid-viewer-viewport svg'), range = document.createRange();
              range.selectNodeContents(svg);
              getSelection().removeAllRanges(); getSelection().addRange(range);
              return { projected: window.transcriptSelectionText(), copied: __transcriptSmoke.copyEvent() };
            })()
            """);
        string completeViewerText = completeViewerCopy.GetProperty("projected").GetString()!;
        Check(completeViewerText == completeViewerCopy.GetProperty("copied").GetString()
            && completeViewerText.Contains("VIEWER_SELECTED", StringComparison.Ordinal)
            && completeViewerText.Contains("VIEWER_DONE", StringComparison.Ordinal)
            && !completeViewerText.Contains("diagram-safe-", StringComparison.Ordinal)
            && !completeViewerText.Contains("font-family", StringComparison.Ordinal)
            && !completeViewerText.Contains("fill:", StringComparison.Ordinal)
            && !completeViewerText.Contains("flowchart", StringComparison.Ordinal),
            "selecting fullscreen SVG contents copies visible labels without source substitution or ancestorless SVG style definitions");
        var partialViewerCopy = await EvalAsync<JsonElement>("""
            (() => {
              const svg = document.querySelector('.mermaid-viewer-viewport svg');
              const label = __transcriptSmoke.textNode(svg, 'VIEWER_SELECTED'), range = document.createRange();
              range.setStart(svg, 0); range.setEnd(label, 6);
              getSelection().removeAllRanges(); getSelection().addRange(range);
              return { projected: window.transcriptSelectionText(), copied: __transcriptSmoke.copyEvent() };
            })()
            """);
        Check(partialViewerCopy.GetProperty("projected").GetString() == "VIEWER"
            && partialViewerCopy.GetProperty("copied").GetString() == "VIEWER",
            "a fullscreen selection starting at SVG children and ending inside a label copies only selected visible letters");
        _metrics["diagramViewerCopy"] = new { completeViewerCopy, partialViewerCopy };
        await EvalAsync<bool>("(() => { window.kynxaDiagrams.closeViewer(); return true; })()");
    }
}

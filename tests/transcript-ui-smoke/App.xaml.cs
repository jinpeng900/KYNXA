using System.Diagnostics;
using System.Text.Json;
using KYNXA_Desktop.Controls;
using KYNXA_Desktop.Models.UI;
using KYNXA_Desktop.Services;
using KYNXA_Desktop.ViewModels;
using Microsoft.UI.Xaml;

namespace TranscriptUiSmoke;

public partial class App : Application
{
    private readonly string _result = Path.Combine(Path.GetTempPath(), "kynxa-transcript-smoke.txt");
    private readonly Dictionary<string, object> _metrics = new();
    private Window? _window;
    private ConversationTranscript _transcript = null!;
    private int _checks;

    public App()
    {
        Environment.SetEnvironmentVariable("KYNXA_DATA_HOME", Path.Combine(Path.GetTempPath(), "kynxa-transcript-data-" + Guid.NewGuid().ToString("N")));
        InitializeComponent();
        UnhandledException += (_, e) => File.WriteAllText(_result, "FAIL: " + e.Exception);
    }

    protected override void OnLaunched(LaunchActivatedEventArgs args)
    {
        var cold = Stopwatch.StartNew();
        if (Environment.GetCommandLineArgs().Contains("--work-tabs-only", StringComparer.Ordinal))
        {
            LaunchWorkTabsChecks(cold);
            return;
        }
        if (Environment.GetCommandLineArgs().Any(argument => argument is "--workspace-preview-only" or "--workspace-chrome-only"))
        {
            LaunchWorkspacePreviewChecks(cold);
            return;
        }
        if (Environment.GetCommandLineArgs().Contains("--prompt-queue-only", StringComparer.Ordinal))
        {
            LaunchPromptQueueChecks(cold);
            return;
        }
        if (Environment.GetCommandLineArgs().Contains("--unknown-outcome-only", StringComparer.Ordinal))
        {
            LaunchUnknownOutcomeChecks(cold);
            return;
        }
        _transcript = new ConversationTranscript();
        _window = new Window { Title = "KYNXA DOM transcript smoke", Content = _transcript };
        KYNXA_Desktop.Services.AppearanceService.Apply(KYNXA_Desktop.Services.AppearanceService.DefaultPaletteId);
        KYNXA_Desktop.Services.AppearanceService.TrackWindow(_window);
        _window.AppWindow.Resize(new Windows.Graphics.SizeInt32(980, 850));
        _window.Closed += (_, _) => _transcript.Dispose();
        if (Environment.GetCommandLineArgs().Any(argument => argument is "--retrieval-tools-only" or "--reply-timing-only" or "--message-time-cache-only" or "--render-performance-only" or "--steering-reply-only" or "--message-attachments-only" or "--content-reading-only" or "--appearance-reading-only"))
        {
            _window.AppWindow.Show(activateWindow: false);
        }
        else
        {
            _window.Activate();
        }
        _ = RunChecksAsync(cold);
    }

    private static ConversationMessageViewModel Message(Guid chat, string role, string text, string status = "completed") =>
        new(chat, new ChatMessageState { Role = role, Content = text, Status = status });

    private async Task RunChecksAsync(Stopwatch cold)
    {
        File.WriteAllText(_result, "RUNNING: DOM transcript checks");
        try
        {
            await _transcript.Ready.WaitAsync(TimeSpan.FromSeconds(40));
            _metrics["initializationMs"] = cold.ElapsedMilliseconds;
            await _transcript.Browser.ExecuteScriptAsync(File.ReadAllText(Path.Combine(AppContext.BaseDirectory, "Checks.js")));
            if (Environment.GetCommandLineArgs().Contains("--message-attachments-only", StringComparer.Ordinal))
            {
                await CheckMessageAttachmentsAsync();
                _metrics["checks"] = _checks;
                _metrics["mode"] = "message-attachments";
                File.WriteAllText(Path.Combine(Path.GetTempPath(), "kynxa-transcript-smoke.json"), JsonSerializer.Serialize(_metrics));
                File.WriteAllText(_result, $"PASS: {_checks} attachment checks; authorized image content, pending upload, file cards, native click, copy, history reload and narrow layout.");
                return;
            }
            if (Environment.GetCommandLineArgs().Contains("--steering-reply-only", StringComparer.Ordinal))
            {
                await CheckSteeringReplyAsync();
                _metrics["checks"] = _checks;
                _metrics["mode"] = "steering-reply-only";
                File.WriteAllText(Path.Combine(Path.GetTempPath(), "kynxa-transcript-smoke.json"), JsonSerializer.Serialize(_metrics, new JsonSerializerOptions { WriteIndented = true }));
                File.WriteAllText(_result, $"PASS: {_checks} native steering-reply checks; prior output retained above guidance, continuation ordering, final Markdown and formal-history reload.");
                return;
            }
            if (Environment.GetCommandLineArgs().Contains("--api-final-only", StringComparer.Ordinal))
            {
                await CheckApiFinalAnswersAsync();
                _metrics["checks"] = _checks;
                File.WriteAllText(Path.Combine(Path.GetTempPath(), "kynxa-transcript-smoke.json"), JsonSerializer.Serialize(_metrics, new JsonSerializerOptions { WriteIndented = true }));
                File.WriteAllText(_result, $"PASS: {_checks} native real-API final-answer checks; full final Markdown, task outcome isolation, continuation projection, hidden tool progress, native clipboard, export and reopened conversation.");
                return;
            }
            if (Environment.GetCommandLineArgs().Contains("--clipboard-persistence-only", StringComparer.Ordinal)
                || Environment.GetCommandLineArgs().Contains("--clipboard-persistence-read-only", StringComparer.Ordinal))
            {
                bool readAfterExit = Environment.GetCommandLineArgs().Contains("--clipboard-persistence-read-only", StringComparer.Ordinal);
                await CheckClipboardPersistenceAsync(readAfterExit);
                _metrics["checks"] = _checks;
                File.WriteAllText(Path.Combine(Path.GetTempPath(), "kynxa-transcript-smoke.json"), JsonSerializer.Serialize(_metrics, new JsonSerializerOptions { WriteIndented = true }));
                File.WriteAllText(_result, $"PASS: {_checks} clipboard persistence {(readAfterExit ? "reader" : "writer")} checks.");
                return;
            }
            if (Environment.GetCommandLineArgs().Contains("--language-navigation-only", StringComparer.Ordinal))
            {
                await CheckLiveLanguageAsync();
                await CheckRapidLanguageNavigationAsync();
                _metrics["checks"] = _checks;
                File.WriteAllText(Path.Combine(Path.GetTempPath(), "kynxa-transcript-smoke.json"), JsonSerializer.Serialize(_metrics, new JsonSerializerOptions { WriteIndented = true }));
                File.WriteAllText(_result, $"PASS: {_checks} native language-navigation checks; localized controls, strict source/selection preservation and pending viewport mounting.");
                return;
            }
            if (Environment.GetCommandLineArgs().Contains("--body-search-only", StringComparer.Ordinal))
            {
                await CheckBodySearchAsync();
                await CheckNativeMergeAsync();
                _metrics["checks"] = _checks;
                File.WriteAllText(Path.Combine(Path.GetTempPath(), "kynxa-transcript-smoke.json"), JsonSerializer.Serialize(_metrics, new JsonSerializerOptions { WriteIndented = true }));
                File.WriteAllText(_result, $"PASS: {_checks} native body-search checks; literal text and styled spans, code/table/quote/Mermaid source, selection and clipboard, folded navigation, cache restoration, streaming, localization, theme and layout; loaded-page virtualization, recycled reading preferences, long-conversation anchors and native history navigation.");
                return;
            }
            if (Environment.GetCommandLineArgs().Contains("--diagram-export-only", StringComparer.Ordinal))
            {
                await CheckDiagramExportAsync();
                _metrics["checks"] = _checks;
                File.WriteAllText(Path.Combine(Path.GetTempPath(), "kynxa-transcript-smoke.json"), JsonSerializer.Serialize(_metrics, new JsonSerializerOptions { WriteIndented = true }));
                File.WriteAllText(_result, $"PASS: {_checks} native diagram-export checks; complete SVG/PNG files, native validation and writes, theme snapshots, local receipts, cancellation, failure, navigation, selection and bounded images.");
                return;
            }
            if (Environment.GetCommandLineArgs().Contains("--appearance-reading-only", StringComparer.Ordinal))
            {
                await CheckAppearanceReadingAsync();
                _metrics["checks"] = _checks;
                File.WriteAllText(Path.Combine(Path.GetTempPath(), "kynxa-transcript-smoke.json"), JsonSerializer.Serialize(_metrics, new JsonSerializerOptions { WriteIndented = true }));
                File.WriteAllText(_result, $"PASS: {_checks} native appearance-reading checks; semantic themes, live font sizing, exact source and selection, cached reading anchors, twelve-line preview and diagrams.");
                return;
            }
            if (Environment.GetCommandLineArgs().Contains("--content-reading-only", StringComparer.Ordinal))
            {
                await CheckContentReadingAsync();
                _metrics["checks"] = _checks;
                File.WriteAllText(Path.Combine(Path.GetTempPath(), "kynxa-transcript-smoke.json"), JsonSerializer.Serialize(_metrics, new JsonSerializerOptions { WriteIndented = true }));
                File.WriteAllText(_result, $"PASS: {_checks} native content-reading checks; table TSV/Markdown clipboard and local receipts, long-block wrap/fold, selection, scrolling, streaming, cached navigation, localization and 480/980 layout.");
                return;
            }
            if (Environment.GetCommandLineArgs().Contains("--format-only", StringComparer.Ordinal))
            {
                await CheckOutputFormatAndResizeAsync();
                _metrics["checks"] = _checks;
                File.WriteAllText(Path.Combine(Path.GetTempPath(), "kynxa-transcript-smoke.json"), JsonSerializer.Serialize(_metrics, new JsonSerializerOptions { WriteIndented = true }));
                File.WriteAllText(_result, $"PASS: {_checks} native output-format checks; 480/980/1600 code scrolling, prose/table wrapping, exact source and native clipboard, shell, empty txt, long txt, ordinary text, quotes, special symbols and blank-line boundaries. Preview: kynxa-transcript-output-format.png.");
                return;
            }
            if (Environment.GetCommandLineArgs().Contains("--block-copy-only", StringComparer.Ordinal))
            {
                await CheckBlockCopyAsync();
                _metrics["checks"] = _checks;
                File.WriteAllText(Path.Combine(Path.GetTempPath(), "kynxa-transcript-smoke.json"), JsonSerializer.Serialize(_metrics, new JsonSerializerOptions { WriteIndented = true }));
                File.WriteAllText(_result, $"PASS: {_checks} native content-block copy checks; exact visible source, clipboard acknowledgement, streaming, selection, cache, localization, palettes, keyboard and narrow layout.");
                return;
            }
            if (Environment.GetCommandLineArgs().Contains("--typography-only", StringComparer.Ordinal))
            {
                await CheckTypographyAsync();
                _metrics["checks"] = _checks;
                File.WriteAllText(Path.Combine(Path.GetTempPath(), "kynxa-transcript-smoke.json"), JsonSerializer.Serialize(_metrics, new JsonSerializerOptions { WriteIndented = true }));
                File.WriteAllText(_result, $"PASS: {_checks} native transcript typography checks; actual platform glyph fonts, Chinese/English UI, monospaced code, math, diagram labels and narrow layout.");
                return;
            }
            if (Environment.GetCommandLineArgs().Contains("--diagrams-only", StringComparer.Ordinal))
            {
                await CheckDiagramsAsync();
                _metrics["checks"] = _checks;
                File.WriteAllText(Path.Combine(Path.GetTempPath(), "kynxa-transcript-smoke.json"), JsonSerializer.Serialize(_metrics, new JsonSerializerOptions { WriteIndented = true }));
                File.WriteAllText(_result, $"PASS: {_checks} native Mermaid diagram checks; bundled SVG types, streaming fences, safe source fallback, native acknowledged copy, palettes, resize, zoom/pan, fullscreen Escape and conversation isolation.");
                return;
            }
            if (Environment.GetCommandLineArgs().Contains("--message-time-cache-only", StringComparer.Ordinal))
            {
                await CheckMessageTimeCacheAsync();
                _metrics["checks"] = _checks;
                File.WriteAllText(Path.Combine(Path.GetTempPath(), "kynxa-transcript-smoke.json"), JsonSerializer.Serialize(_metrics, new JsonSerializerOptions { WriteIndented = true }));
                File.WriteAllText(_result, $"PASS: {_checks} native message end-time cache checks; restart restoration, conversation/root isolation, final-content validation, retry, deletion/undo, write failure and rapid navigation without changing formal messages.");
                return;
            }
            if (Environment.GetCommandLineArgs().Contains("--metadata-only", StringComparer.Ordinal))
            {
                await CheckMessageMetadataAsync();
                _metrics["checks"] = _checks;
                File.WriteAllText(Path.Combine(Path.GetTempPath(), "kynxa-transcript-smoke.json"), JsonSerializer.Serialize(_metrics, new JsonSerializerOptions { WriteIndented = true }));
                File.WriteAllText(_result, $"PASS: {_checks} persistent message time checks; no message hover popup, exact terminal states, 480/980 layout, localization, selection and native acknowledged copy.");
                return;
            }
            if (Environment.GetCommandLineArgs().Contains("--render-performance-only", StringComparer.Ordinal))
            {
                await CheckRenderPerformanceAsync();
                _metrics["checks"] = _checks;
                _metrics["mode"] = "render-performance-only";
                File.WriteAllText(Path.Combine(Path.GetTempPath(), "kynxa-transcript-performance.json"),
                    JsonSerializer.Serialize(_metrics, new JsonSerializerOptions { WriteIndented = true }));
                File.WriteAllText(_result, $"PASS: {_checks} native transcript performance checks; changed-row transport, selected patch convergence, preserved DOM and user-driven history admission.");
                return;
            }
            if (Environment.GetCommandLineArgs().Contains("--reply-timing-only", StringComparer.Ordinal))
            {
                await CheckReplyTimingAsync();
                File.WriteAllText(_result, $"PASS: {_checks} native reply timing checks; preparation hidden, monotonic live duration, selection-safe completion, language/navigation, retry and terminal cleanup.");
                return;
            }
            if (Environment.GetCommandLineArgs().Contains("--retrieval-tools-only", StringComparer.Ordinal))
            {
                await CheckRetrievalToolsAsync();
                File.WriteAllText(_result, $"PASS: {_checks} native retrieval transcript checks; unified website links, local source labels, live translation and final convergence.");
                return;
            }
            if (Environment.GetCommandLineArgs().Contains("--inline-math-only", StringComparer.Ordinal))
            {
                await CheckLongInlineMathAsync();
                File.WriteAllText(_result, $"PASS: {_checks} long inline math checks.");
                return;
            }
            if (Environment.GetCommandLineArgs().Contains("--computer-tools-only", StringComparer.Ordinal))
            {
                await CheckComputerToolsAsync();
                File.WriteAllText(_result, $"PASS: {_checks} native computer transcript checks. Preview: kynxa-transcript-computer-tools.png.");
                return;
            }
            if (Environment.GetCommandLineArgs().Contains("--multiline-only", StringComparer.Ordinal))
            {
                await CheckMultilineRepliesAsync();
                File.WriteAllText(_result, $"PASS: {_checks} multiline reply checks. Preview: kynxa-transcript-multiline.png.");
                return;
            }
            var longChat = Guid.NewGuid();
            string table = "COLD_START\n\n| 名称 | 公式 | 符号解释 | 条件 |\n| --- | --- | --- | --- |\n"
                + string.Join("\n", Enumerable.Range(1, 50).Select(index =>
                    $"| 公式 {index} | $v_{{{index}}}=\\frac{{s}}{{t}}$ | $s_{{{index}}}$ 位移，$t_{{{index}}}$ 时间 | 匀速运动 |"))
                + "\n\n```python\nprint('cold code')\n```\n\nCOLD_END";
            var longRows = new List<ConversationMessageViewModel>
            {
                Message(longChat, "user", "USER_COLD 请展示物理公式"),
                Message(longChat, "assistant", table),
            };
            _transcript.ShowConversation(longChat, longRows);
            await WaitAsync("document.querySelectorAll('.katex').length === 150 && document.fonts.status === 'loaded'", "150 formulas render after cold initialization");
            _metrics["cold150IncludingInitializationMs"] = cold.ElapsedMilliseconds;
            Check(await EvalAsync<bool>("document.querySelectorAll('.math img, .katex img').length === 0"), "formulas use native DOM, without images");
            Check(await EvalAsync<bool>("document.querySelectorAll('.message-body table tbody tr').length === 50"), "all table rows are real HTML cells");
            await WaitAsync("__transcriptSmoke.bottomDistance() < 4", "opening long chat starts at bottom");

            var warm = Stopwatch.StartNew();
            longRows[1].Message.Content += "\n\nWARM_APPEND";
            longRows[1].Refresh();
            await WaitAsync("__transcriptSmoke.bodyText().includes('WARM_APPEND') && document.querySelectorAll('.katex').length === 150", "warm append preserves all formulas");
            _metrics["warmAppend150Ms"] = warm.ElapsedMilliseconds;

            await CheckSelectionAndCopyAsync();
            await CheckLiveLanguageAsync();
            await CheckRapidLanguageNavigationAsync();
            _transcript.ShowConversation(longChat, longRows);
            await WaitAsync("document.querySelectorAll('.katex').length === 150 && __transcriptSmoke.bottomDistance() < 4", "reopening a chat returns to bottom");
            await EvalAsync<bool>("__transcriptSmoke.scrollUp()");
            await Task.Delay(150);
            double previousTop = await EvalAsync<double>("__transcriptSmoke.scroller().scrollTop");
            longRows[1].Message.Content += "\n\nSCROLLED_APPEND";
            longRows[1].Refresh();
            await WaitAsync("__transcriptSmoke.bodyText().includes('SCROLLED_APPEND')", "updates arrive while reading earlier content");
            await Task.Delay(150);
            double nextTop = await EvalAsync<double>("__transcriptSmoke.scroller().scrollTop");
            Check(Math.Abs(previousTop - nextTop) < 2, "manual upward scroll is preserved during append");
            await EvalAsync<bool>("__transcriptSmoke.scrollBottom()");
            await Task.Delay(100);
            _transcript.BeforeSend();
            longRows[1].Message.Content += "\n\nFOLLOW_APPEND\n\n" + string.Join("\n\n", Enumerable.Repeat("继续回复", 8));
            longRows[1].Refresh();
            await WaitAsync("__transcriptSmoke.bodyText().includes('FOLLOW_APPEND') && __transcriptSmoke.bottomDistance() < 4", "send from bottom follows appended output");

            await CheckConversationSwitchingAsync(longChat, longRows);
            await CheckToolActivitiesAsync();
            await CheckWebsiteActivitiesAsync();
            await CheckRetrievalToolsAsync();
            CheckPresentationSources();
            await CheckAssistantTimelineAsync();
            await CheckPartialProgressAsync();
            await CheckProgressScrollAnchorAsync();
            await CheckReplyTimingAsync();
            await CheckTranscriptActionsAsync();
            await CheckMessageMetadataAsync();
            await CheckMessageTimeCacheAsync();
            await CheckTypographyAsync();
            await CheckDiagramsAsync();
            await CheckDiagramExportAsync();
            await CheckBlockCopyAsync();
            await CheckContentReadingAsync();
            await CheckBodySearchAsync();
            await CheckNativeMergeAsync();
            await CheckMultilineRepliesAsync();
            await CheckOutputFormatAndResizeAsync();
            await CheckLongInlineMathAsync();
            await CaptureVisualPreviewAsync();
            bool pointerRequested = Environment.GetCommandLineArgs().Contains("--pointer");
            _metrics["pointerValidation"] = pointerRequested ? "requested" : "not requested; run --pointer for the isolated CDP drag diagnostic";
            if (pointerRequested)
            {
                await CheckPointerSelectionAsync();
                _metrics["pointerValidation"] = "passed";
            }

            _metrics["checks"] = _checks;
            _metrics["finalState"] = await EvalAsync<JsonElement>("window.transcriptState()");
            File.WriteAllText(Path.Combine(Path.GetTempPath(), "kynxa-transcript-smoke.json"), JsonSerializer.Serialize(_metrics, new JsonSerializerOptions { WriteIndented = true }));
            File.WriteAllText(_result, $"PASS: {_checks} DOM transcript checks; cold150={_metrics["cold150IncludingInitializationMs"]}ms, warmAppend={_metrics["warmAppend150Ms"]}ms. Native tables/math, exact code copy, live language switching, 480/980/1600 code scrolling and prose wrapping, output-format boundaries, selected-content preservation, deferred final updates, opening/following scroll. Pointer diagnostic: {(pointerRequested ? "passed" : "not requested (--pointer)")}. Preview: kynxa-transcript-preview.png.");
        }
        catch (Exception error)
        {
            File.WriteAllText(_result, "FAIL: " + error);
            _metrics["checksBeforeFailure"] = _checks;
            _metrics["failure"] = error.Message;
            File.WriteAllText(Path.Combine(Path.GetTempPath(), "kynxa-transcript-smoke.json"), JsonSerializer.Serialize(_metrics, new JsonSerializerOptions { WriteIndented = true }));
            try { File.WriteAllText(Path.Combine(Path.GetTempPath(), "kynxa-transcript-failure.json"), await _transcript.Browser.ExecuteScriptAsync("JSON.stringify({state:window.transcriptState?.(),html:document.body.innerHTML})")); } catch { }
        }
        finally
        {
            if (_pointerWindowPosition is { } position) _window?.AppWindow.Move(position);
            if (!Environment.GetCommandLineArgs().Contains("--keep-open")) _window?.Close();
        }
    }

    private async Task CheckRenderPerformanceAsync()
    {
        var chat = Guid.NewGuid();
        var rows = Enumerable.Range(0, 200).Select(index => Message(chat, index % 2 == 0 ? "user" : "assistant",
            $"ROW_{index} 原文 " + new string('文', 300))).ToList();
        rows[^1].Message.Content += new string('文', 40000);
        int requestedHistory = 0;
        EventHandler<Guid> history = (_, id) => { if (id == chat) requestedHistory++; };
        _transcript.HistoryRequested += history;
        try
        {
            await EvalAsync<bool>("""
                (() => {
                  window.__renderCommands = []; window.__pendingMarkdown = new Set(); window.__readablePreview = false;
                  window.__previewObserver = new MutationObserver(() => {
                    if (__pendingMarkdown.size && __transcriptSmoke.bodyText().includes('ROW_199')) __readablePreview = true;
                  });
                  __previewObserver.observe(document.getElementById('messages'), {childList:true,subtree:true});
                  window.chrome.webview.addEventListener('message', event => {
                    const value = event.data;
                    if (value.type !== 'render' && value.type !== 'renderPatch') return;
                    __renderCommands.push({type:value.type,count:value.messages.length,last:value.messages.at(-1)?.content,lastHtmlLength:value.messages.at(-1)?.html?.length || 0});
                    if (value.type === 'render') __pendingMarkdown.clear();
                    for (const message of value.messages) {
                      const waiting = (message.html || '').includes('markdown-pending') ||
                        message.assistantSegments?.some(segment => (segment.html || '').includes('markdown-pending'));
                      if (waiting) __pendingMarkdown.add(message.id); else __pendingMarkdown.delete(message.id);
                    }
                    if (__pendingMarkdown.size && __transcriptSmoke.bodyText().includes('ROW_199')) __readablePreview = true;
                  });
                  return true;
                })()
                """);
            _transcript.ShowConversation(chat, rows);
            Check(!(bool)typeof(ConversationTranscript).GetField("_hasPublishedSnapshot",
                System.Reflection.BindingFlags.Instance | System.Reflection.BindingFlags.NonPublic)!.GetValue(_transcript)!,
                "first-page projection is queued outside the caller's send/input handler");
            await WaitAsync("window.transcriptState().messageCount === 200 && __transcriptSmoke.bottomDistance() < 4", "paged long transcript opens at the latest messages");
            Check(await EvalAsync<bool>("__renderCommands[0].count === 4 && __renderCommands[0].last.startsWith('ROW_199')"),
                "first projection contains the latest four readable rows before older history is sent");
            Check(await EvalAsync<bool>("__renderCommands[0].last.length > 40000 && __renderCommands[0].lastHtmlLength < 9000"),
                "a long latest reply retains complete source but initially lays out only its readable tail");
            await WaitAsync("window.__pendingMarkdown.size === 0", "background batches complete the initial Markdown page");
            await EvalAsync<bool>("(() => { __previewObserver.disconnect(); return true; })()");
            await WaitAsync("window.transcriptState().mountedMessageCount < 80 && document.querySelectorAll('article[data-virtualized]').length > 100", "offscreen messages release their body DOM into measured placeholders");
            _metrics["loaded200Mounted"] = await EvalAsync<int>("window.transcriptState().mountedMessageCount");
            Check(requestedHistory == 0, "initial bottom navigation does not request earlier pages");
            await EvalAsync<bool>("(() => { window.__unchangedArticle = document.querySelector('article.message'); return true; })()");
            rows[^1].Message.Content += " PATCH_VISIBLE";
            rows[^1].Refresh();
            await WaitAsync("__transcriptSmoke.bodyText().includes('PATCH_VISIBLE')", "changed message is rendered");
            Check(await EvalAsync<bool>("__renderCommands.at(-1).type === 'renderPatch' && __renderCommands.at(-1).count === 1"), "one changed row is transported instead of all 200 loaded messages");
            Check(await EvalAsync<bool>("window.__unchangedArticle === document.querySelector('article.message')"), "unchanged article retains its DOM identity");

            await EvalAsync<bool>("(() => { window.dispatchEvent(new WheelEvent('wheel',{deltaY:-120,bubbles:true})); window.scrollTo({top:0,behavior:'instant'}); return true; })()");
            await WaitAsync("__transcriptSmoke.bodyText().includes('ROW_0')", "scrolling mounts the original earlier message");
            var firstHistoryDeadline = DateTime.UtcNow.AddSeconds(5);
            while (requestedHistory == 0 && DateTime.UtcNow < firstHistoryDeadline) await Task.Delay(25);
            Check(requestedHistory == 1, "user upward navigation loads one earlier page");
            _transcript.ShowConversation(chat, rows, openAtBottom: true);
            await Task.Delay(120);
            Check(await EvalAsync<bool>("scrollY < 100 && !window.transcriptState().following"),
                "a late bottom hint cannot override the user's upward navigation");
            string selected = await EvalAsync<string>("(() => { const node = __transcriptSmoke.textNode(document.querySelector('article.message'), 'ROW_0'); getSelection().setBaseAndExtent(node,0,node,20); return window.transcriptSelectionText(); })()");
            _transcript.ShowConversation(chat, rows, openAtBottom: true);
            await WaitAsync("window.transcriptState().pending", "late refresh waits for the active selection");
            Check(await EvalAsync<string>("window.transcriptSelectionText()") == selected,
                "a late first-page bottom hint leaves selected original text intact");
            rows[^1].Message.Content += " PATCH_SELECTED_A";
            rows[^1].Refresh();
            await WaitAsync("window.transcriptState().pending", "first selected patch is deferred");
            rows[^3].Message.Content += " PATCH_SELECTED_B";
            rows[^3].Refresh();
            await WaitAsync("__renderCommands.at(-1).type === 'renderPatch'", "subsequent selected row uses a patch");
            await Task.Delay(120);
            Check(await EvalAsync<string>("window.transcriptSelectionText()") == selected, "selected original content remains stable across patches");
            _transcript.ClearSelection();
            await EvalAsync<bool>("(() => { window.scrollTo({top:document.documentElement.scrollHeight,behavior:'instant'}); return true; })()");
            await WaitAsync("__transcriptSmoke.bodyText().includes('PATCH_SELECTED_A') && !window.transcriptState().pending", "clearing selection applies the visible deferred update");
            Check(await EvalAsync<bool>("(() => { const range=document.createRange();range.selectNodeContents(document.getElementById('messages'));getSelection().removeAllRanges();getSelection().addRange(range);const text=window.transcriptSelectionText();getSelection().removeAllRanges();return text.includes('PATCH_SELECTED_A') && text.includes('PATCH_SELECTED_B');})()"),
                "both deferred updates remain in complete selectable source even when the older row is virtualized");

            await EvalAsync<bool>("(() => { window.scrollTo({top:0,behavior:'instant'}); return true; })()");
            await Task.Delay(120);
            Check(requestedHistory == 1, "programmatic scroll does not load the entire history");
            await EvalAsync<bool>("(() => { window.chrome.webview.postMessage({type:'history',conversationId:'00000000-0000-0000-0000-000000000000'}); return true; })()");
            await Task.Delay(80);
            Check(requestedHistory == 1, "history request from another conversation is ignored");
            // Dispatching a synthetic wheel has no default scrolling; move with the upward input intent.
            // 合成滚轮事件不执行默认滚动；同时移动视口，避免把仍在底部的事件误当作顶部翻页。
            await EvalAsync<bool>("(() => { window.dispatchEvent(new WheelEvent('wheel',{deltaY:-120,bubbles:true})); window.scrollTo({top:0,behavior:'instant'}); return true; })()");
            await WaitAsync("scrollY <= 1 && !window.transcriptState().following", "upward history navigation reaches the top without bottom following");
            var deadline = DateTime.UtcNow.AddSeconds(5);
            while (requestedHistory == 1 && DateTime.UtcNow < deadline) await Task.Delay(25);
            Check(requestedHistory == 2, "user upward input at the top requests one history page");
            await Task.Delay(120);
            Check(requestedHistory == 2, "no further page is requested without another user action");

            var cacheChat = Guid.NewGuid();
            var cacheRows = Enumerable.Range(0, 1100).Select(index => Message(cacheChat, "assistant", $"CACHE_{index} 原文")).ToList();
            _transcript.ShowConversation(cacheChat, cacheRows);
            await WaitAsync("window.transcriptState().messageCount === 1100 && __transcriptSmoke.bottomDistance() < 4", "history exceeding the HTML cache opens with a bounded DOM window");
            await WaitAsync("window.__pendingMarkdown.size === 0", "background Markdown batches finish before measuring a changed row");
            long parsedBefore = TranscriptMarkdown.ParseCount;
            cacheRows[^1].Message.Content += " CACHE_CHANGE"; cacheRows[^1].Refresh();
            await WaitAsync("__transcriptSmoke.bodyText().includes('CACHE_CHANGE')", "a changed row is visible after historical HTML eviction");
            Check(TranscriptMarkdown.ParseCount - parsedBefore == 1, "evicted unchanged history is not reparsed on a stream refresh");
            _metrics["oneChangedRowMarkdownParses"] = TranscriptMarkdown.ParseCount - parsedBefore;
            Check(await EvalAsync<bool>("window.transcriptState().mountedMessageCount < 100"), "1100 historical bodies do not remain mounted");
            _metrics["loaded1100Mounted"] = await EvalAsync<int>("window.transcriptState().mountedMessageCount");

            await EvalAsync<bool>("(() => { const range=document.createRange(); range.selectNodeContents(document.getElementById('messages')); getSelection().removeAllRanges();getSelection().addRange(range);return true; })()");
            string allCopied = await EvalAsync<string>("window.transcriptSelectionText()");
            Check(allCopied.Contains("CACHE_0 原文") && allCopied.Contains("CACHE_1099 原文 CACHE_CHANGE"), "copying across unloaded placeholders includes both history endpoints");
            _transcript.ClearSelection();

            var historyDirections = new List<string>();
            EventHandler<ConversationHistoryRequest> historyNavigation = (_, request) =>
            { if (request.ConversationId == cacheChat) historyDirections.Add(request.Direction); };
            _transcript.HistoryNavigationRequested += historyNavigation;
            try
            {
                _transcript.SetHistoryAvailability(cacheChat, hasOlder: true, hasNewer: true);
                await WaitAsync("!document.getElementById('jump-to-latest').hidden", "an older page window offers navigation to durable latest messages");
                await EvalAsync<bool>("(() => { document.getElementById('jump-to-latest').click();return true; })()");
                var navigationDeadline = DateTime.UtcNow.AddSeconds(5);
                while (historyDirections.Count == 0 && DateTime.UtcNow < navigationDeadline) await Task.Delay(25);
                Check(historyDirections.SequenceEqual(["latest"]), "jump to latest requests a latest page instead of the end of an old window");
                await Task.Delay(150);
                await EvalAsync<bool>("(() => { window.dispatchEvent(new WheelEvent('wheel',{deltaY:120,bubbles:true}));return true; })()");
                while (historyDirections.Count < 2 && DateTime.UtcNow < navigationDeadline) await Task.Delay(25);
                Check(historyDirections.SequenceEqual(["latest", "newer"]), "user downward navigation requests one newer page");
                await Task.Delay(150);
                Check(historyDirections.Count == 2, "newer navigation does not automatically walk through all durable pages");
            }
            finally { _transcript.HistoryNavigationRequested -= historyNavigation; }

            var streamChat = Guid.NewGuid();
            var streamed = Message(streamChat, "assistant", string.Join("\n\n", Enumerable.Range(0, 300)
                .Select(index => $"STABLE_{index} completed paragraph")) + "\n\nSTREAM_TAIL", "streaming");
            _transcript.ShowConversation(streamChat, [streamed]);
            await WaitAsync("__transcriptSmoke.bodyText().includes('STREAM_TAIL') && document.querySelector('.message-body p') !== null", "a long streamed response initially renders its stable blocks");
            await EvalAsync<bool>("(() => { window.__stablePrefixNode=document.querySelector('.message-body p');return true; })()");
            long streamingCharactersBefore = TranscriptMarkdown.ParsedCharacters;
            streamed.Message.Content += " APPEND_TAIL"; streamed.Refresh();
            await WaitAsync("__transcriptSmoke.bodyText().includes('APPEND_TAIL')", "an appended streaming tail reaches the page");
            Check(TranscriptMarkdown.ParsedCharacters - streamingCharactersBefore < 100,
                "stream refresh parses its short tail without reparsing the 300 completed paragraphs");
            _metrics["streamTotalCharacters"] = streamed.Message.Content.Length;
            _metrics["streamRefreshParsedCharacters"] = TranscriptMarkdown.ParsedCharacters - streamingCharactersBefore;
            Check(await EvalAsync<bool>("window.__stablePrefixNode===document.querySelector('.message-body p')"),
                "streaming retains the existing first paragraph DOM node");
            streamed.Message.Content += "\n\n```python\nprint('STREAM_CODE')\n```\n\n$x=1$"; streamed.Refresh();
            await WaitAsync("__transcriptSmoke.bodyText().includes('STREAM_CODE') && document.querySelector('.katex') !== null", "new stable code and math blocks are appended without dropping the preserved prefix");
            streamed.Message.Status = "completed"; streamed.Refresh();
            await WaitAsync("!window.transcriptState().liveElapsedCount && document.querySelector('pre[data-code-complete=true]') !== null", "final response renders authoritative Markdown and complete code state");

            var taskChat = Guid.NewGuid();
            var taskRow = Message(taskChat, "assistant", "FINAL_WITH_FAILED_CHECKS");
            taskRow.Message.AssistantSegments =
            [
                new("task-progress", 1, 0, "commentary", "completed", "CHECK_PROGRESS_RETAINED", ""),
                new("task-final", 2, 2, "final_answer", "completed", "FINAL_WITH_FAILED_CHECKS", "")
            ];
            taskRow.Message.TaskCompletionState = "verification-failed";
            _transcript.ShowConversation(taskChat, [taskRow]);
            await WaitAsync("!!document.querySelector('.message-status')?.textContent.includes('核验失败') && !__transcriptSmoke.bodyText().includes('CHECK_PROGRESS_RETAINED') && __transcriptSmoke.bodyText().includes('FINAL_WITH_FAILED_CHECKS')", "task verification remains visible while the completed final answer replaces execution progress");
            string originalLanguage = UiText.Language;
            UiText.Initialize("en");
            await WaitAsync("!!document.querySelector('.message-status')?.textContent.includes('Verification failed')", "task outcome controls translate without rewriting the answer");
            UiText.Initialize(originalLanguage);
        }
        finally { _transcript.HistoryRequested -= history; }
    }

    private async Task CheckSelectionAndCopyAsync()
    {
        var chat = Guid.NewGuid();
        var rows = new List<ConversationMessageViewModel>
        {
            Message(chat, "user", "USER_START 用户 **原样** 内容\n  第二行"),
            Message(chat, "assistant", "ASSISTANT_START\n\n公式 $\\omega=2\\pi f$ 和 $\\text{ABCDE}+x$。\n\n| 名称 | 公式 |\n| --- | --- |\n| 速度 | $v=s/t$ |\n\n```python\nprint('hello')\n    return value\n```"),
            Message(chat, "user", "USER_NEXT 继续提问"),
            Message(chat, "assistant", "ASSISTANT_END", "streaming"),
        };
        rows[1].Message.Reasoning = "THINKING_PRIVATE_FIXTURE 这段只在展开时可选";
        rows[1].Message.ReasoningDurationMs = 1000;
        _transcript.ShowConversation(chat, rows);
        await WaitAsync("document.querySelectorAll('#messages > article.message').length === 4 && document.querySelectorAll('.katex').length === 3", "multiple user and assistant DOM messages render");
        Check(await EvalAsync<bool>("document.querySelector('[data-role=user] .message-body').textContent.includes('**原样**')"), "user text remains literal");
        string forward = await EvalAsync<string>("__transcriptSmoke.copyAcross(false)");
        string backward = await EvalAsync<string>("__transcriptSmoke.copyAcross(true)");
        Check(forward == backward, "forward and backward cross-message copies agree");
        Check(forward.Contains("USER_START") && forward.Contains("ASSISTANT_START") && forward.Contains("USER_NEXT") && forward.Contains("ASSISTANT_END"), "copy includes consecutive user and assistant messages");
        Check(forward.Contains(@"\omega=2\pi f") && forward.Contains("print('hello')"), "copy preserves formula source and code");
        Check(forward.Contains("名称\t公式") && forward.Contains("速度\t"), "table copy preserves column and row separators");
        Check(!forward.Contains("THINKING_PRIVATE_FIXTURE") && !forward.Contains("思考过程"), "closed reasoning and action labels stay out of cross-message copy");
        Check(await EvalAsync<string>("__transcriptSmoke.copyUserWhitespace()") == "内容\n  第二行", "partial user copy preserves newlines and indentation");
        Check(await EvalAsync<string>("__transcriptSmoke.copyCodeIndent()") == "    ", "partial code text node preserves indentation");
        Check(await EvalAsync<bool>("!document.querySelector('.reasoning') && !document.getElementById('messages').textContent.includes('THINKING_PRIVATE_FIXTURE')"), "completed process source is absent from final-only DOM and cross-message selection");
        string fullMath = await EvalAsync<string>("__transcriptSmoke.copyFormula(false)");
        Check(fullMath.Contains(@"\omega=2\pi f") && fullMath.Split(@"\omega=2\pi f").Length == 2, "full formula copy contains one original TeX source");
        string partialMath = await EvalAsync<string>("__transcriptSmoke.copyFormula(true)");
        Check(partialMath.Trim() == "BC", "partial formula copy retains only the selected visible characters");
        string frozen = await EvalAsync<string>("__transcriptSmoke.copyAcross(false)");
        rows[^1].Message.Content += " FINAL_UPDATED";
        rows[^1].Message.Status = "completed";
        rows[^1].Refresh();
        await WaitAsync("window.transcriptState().pending", "active selection defers final DOM updates");
        Check(await EvalAsync<string>("window.transcriptSelectionText()") == frozen, "stream completion preserves cross-message selection");
        Check(!await EvalAsync<bool>("__transcriptSmoke.bodyText().includes('FINAL_UPDATED')"), "selected content is not replaced early");
        _transcript.ClearSelection();
        await WaitAsync("__transcriptSmoke.bodyText().includes('FINAL_UPDATED') && !window.transcriptState().pending", "clearing selection applies the final update");
        Check(await EvalAsync<bool>("window.getSelection().isCollapsed"), "host ClearSelection clears the browser range");
        var retried = new TaskCompletionSource<Guid>(TaskCreationOptions.RunContinuationsAsynchronously);
        EventHandler<Guid> retry = (_, id) => retried.TrySetResult(id);
        _transcript.RetryRequested += retry;
        try
        {
            rows[^1].Message.Status = "error";
            rows[^1].Message.Error = "模拟中断";
            rows[^1].SetRetryAllowed(true);
            rows[^1].Refresh();
            await WaitAsync("!!document.querySelector('.retry:not([hidden])')", "failed last reply exposes retry");
            await EvalAsync<bool>("(() => { document.querySelector('.retry:not([hidden])').click(); return true; })()");
            Check(await retried.Task.WaitAsync(TimeSpan.FromSeconds(5)) == rows[^1].Message.Id, "retry action reaches C# with the correct message identity");
        }
        finally { _transcript.RetryRequested -= retry; }
    }

    private async Task<T> EvalAsync<T>(string expression)
    {
        string json = await _transcript.Browser.ExecuteScriptAsync("(() => (" + expression + "))()");
        return JsonSerializer.Deserialize<T>(json)!;
    }

    private async Task WaitAsync(string expression, string description)
    {
        var deadline = DateTime.UtcNow.AddSeconds(30);
        while (DateTime.UtcNow < deadline)
        {
            if (await EvalAsync<bool>(expression)) { _checks++; return; }
            await Task.Delay(25);
        }
        throw new InvalidOperationException(description + " (timed out)");
    }

    private void Check(bool condition, string description)
    {
        if (!condition) throw new InvalidOperationException(description);
        _checks++;
    }
}

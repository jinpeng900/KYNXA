using System.Diagnostics;
using System.Text.Json;
using KYNXA_Desktop.Controls;
using KYNXA_Desktop.Models.UI;
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
        _transcript = new ConversationTranscript();
        _window = new Window { Title = "KYNXA DOM transcript smoke", Content = _transcript };
        _window.AppWindow.Resize(new Windows.Graphics.SizeInt32(980, 850));
        _window.Closed += (_, _) => _transcript.Dispose();
        if (Environment.GetCommandLineArgs().Any(argument => argument is "--retrieval-tools-only" or "--reply-timing-only" or "--message-time-cache-only"))
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
            await CheckBlockCopyAsync();
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

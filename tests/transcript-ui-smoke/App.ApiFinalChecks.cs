using System.Security.Cryptography;
using System.Text.Json;
using KYNXA.Contracts;
using KYNXA_Desktop.Models.UI;
using KYNXA_Desktop.Services;
using KYNXA_Desktop.ViewModels;
using Windows.ApplicationModel.DataTransfer;

namespace TranscriptUiSmoke;

public partial class App
{
    // Read only the explicitly supplied acceptance report; neither this fixture nor its UI can dispatch a model.
    // 只读取明确指定的验收报告；此夹具及其界面均不能派发模型请求。
    private async Task CheckApiFinalAnswersAsync()
    {
        string reportPath = Environment.GetEnvironmentVariable("KYNXA_TRANSCRIPT_API_REPORT")
            ?? throw new InvalidOperationException("KYNXA_TRANSCRIPT_API_REPORT is required.");
        byte[] reportBytes = await File.ReadAllBytesAsync(reportPath);
        using var report = JsonDocument.Parse(reportBytes);
        var options = new JsonSerializerOptions(JsonSerializerDefaults.Web);
        var scenarios = report.RootElement.GetProperty("scenarios");
        Check(scenarios.ValueKind == JsonValueKind.Array && scenarios.GetArrayLength() > 0,
            "the explicit API report contains scenarios");
        _metrics["apiReportPath"] = Path.GetFullPath(reportPath);
        _metrics["apiReportSha256"] = Convert.ToHexString(SHA256.HashData(reportBytes));
        _metrics["sourceApiPassed"] = report.RootElement.TryGetProperty("passed", out var passed) && passed.GetBoolean();
        var evidence = new List<object>();
        int scenarioIndex = 0;
        try
        {
            await _transcript.Browser.CoreWebView2.CallDevToolsProtocolMethodAsync("Emulation.setFocusEmulationEnabled", "{\"enabled\":true}");
            foreach (var scenario in scenarios.EnumerateArray())
            {
                var terminal = scenario.GetProperty("terminal").Deserialize<ChatStreamEvent>(options)
                    ?? throw new InvalidOperationException("The API terminal event could not be decoded.");
                Check(terminal.Type == "completed", "only a received completed reply can certify final-answer delivery");
                var segments = terminal.AssistantSegments ?? [];
                Check(AssistantSegmentRules.IsValidSequence(segments), "the real API segment sequence retains the public contract");
                var ordered = segments.OrderBy(segment => segment.Order).ToArray();
                int finalIndex = Array.FindLastIndex(ordered, segment => segment.Phase == "final_answer"
                    && segment.Status == "completed" && !string.IsNullOrWhiteSpace(segment.Content));
                Check(finalIndex >= 0, "the real API actually delivered a completed final_answer segment");
                int answerStart = finalIndex;
                while (answerStart > 0 && ordered[answerStart - 1] is { IsAnswerContinuation: true,
                    Phase: "commentary", Status: "completed" }) answerStart--;
                string expected = string.Join("\n\n", ordered[answerStart..(finalIndex + 1)]
                    .Select(segment => segment.Content).Where(content => !string.IsNullOrWhiteSpace(content)));
                var tools = scenario.GetProperty("events").EnumerateArray()
                    .Where(value => value.TryGetProperty("tool", out var tool) && tool.ValueKind == JsonValueKind.Object)
                    .Select(value => value.GetProperty("tool").Deserialize<ToolActivity>(options)!)
                    .GroupBy(tool => tool.ToolCallId, StringComparer.Ordinal).Select(group => group.Last()).ToList();
                var reply = new ChatMessageState
                {
                    Id = terminal.RequestId,
                    Role = "assistant",
                    Status = terminal.Type,
                    Content = terminal.Content ?? "",
                    Reasoning = terminal.Reasoning ?? "",
                    AssistantSegments = segments.ToList(),
                    ToolActivities = tools,
                    DurationMs = terminal.DurationMs,
                    CreatedAt = terminal.CreatedAt,
                    TaskCompletionState = ChatMessageState.ReadTaskCompletion(terminal.TaskCompletion)
                };
                string sourceSnapshot = JsonSerializer.Serialize(reply);
                string actualOutcome = reply.TaskCompletionState;
                string prefix = Path.Combine(Path.GetTempPath(), $"kynxa-api-final-{scenarioIndex++}");

                // Compare the raw browser projection as well as the host's production Markdown path.
                // 同时比较浏览器的原始投影和宿主正式 Markdown 渲染路径，防止一端掩盖另一端的差异。
                string rawMessage = JsonSerializer.Serialize(new { role = reply.Role, status = reply.Status,
                    content = reply.Content, assistantSegments = segments, toolActivities = tools,
                    taskCompletionState = actualOutcome }, options);
                var browserProjection = await EvalAsync<JsonElement>(
                    $"window.KynxaMessagePresentation.selectMessagePresentation({rawMessage})");
                Check(browserProjection.GetProperty("mode").GetString() == "final"
                    && browserProjection.GetProperty("content").GetString() == expected
                    && browserProjection.GetProperty("tools").GetArrayLength() == 0,
                    "browser projection includes all marked answer continuations and excludes tool progress");
                var hostProjection = TranscriptPresentation.Select(reply.Role, reply.Status, reply.Content,
                    reply.AssistantSegments, reply.ToolActivities, actualOutcome);
                Check(hostProjection.Mode == "final" && hostProjection.Content == expected
                    && hostProjection.Segments.Length == 1 && hostProjection.Tools.Length == 0,
                    "host projection passes the complete answer through a single Markdown body");

                var row = new ConversationMessageViewModel(terminal.ConversationId, reply);
                _transcript.ClearSelection();
                _transcript.ShowConversation(terminal.ConversationId, [row]);
                await CheckApiAnswerDomAsync(expected);
                await CopyProgressMessageAsync(".assistant .copy-message", expected, "apiFinalCopy" + scenarioIndex);
                Check(await Clipboard.GetContent().GetTextAsync() == expected,
                    "native message copy equals the complete final Markdown exactly");
                string exported = ConversationExport.ToMarkdown("", [reply], "User", "Assistant");
                Check(exported == "## Assistant" + Environment.NewLine + Environment.NewLine
                    + expected + Environment.NewLine + Environment.NewLine,
                    "production export contains the same complete visible final Markdown");
                await File.WriteAllTextAsync(prefix + ".md", exported);
                await SaveApiAnswerEvidenceAsync(prefix, expected);

                // Replay only an outcome label against the real, unchanged terminal text when the API outcome was successful.
                // API 任务已成功时，仅对同一真实终态正文重放部分成果标签；该 UI 检查不改变原始验收结果。
                bool derivedPartialOutcome = actualOutcome != "partial";
                if (derivedPartialOutcome)
                {
                    reply.TaskCompletionState = "partial";
                    row.Refresh();
                    await CheckApiAnswerDomAsync(expected);
                    await ClickAndAwaitNativeCopyAsync("document.querySelector('.assistant .copy-message')", "partial-outcome answer");
                    Check(await Clipboard.GetContent().GetTextAsync() == expected,
                        "a partial task label still copies the full delivered answer");
                    reply.TaskCompletionState = actualOutcome;
                    row.Refresh();
                }

                _transcript.ShowConversation(Guid.NewGuid(), []);
                _transcript.ShowConversation(terminal.ConversationId, [row]);
                await CheckApiAnswerDomAsync(expected);
                Check(JsonSerializer.Serialize(reply) == sourceSnapshot && reply.TaskCompletionState == actualOutcome,
                    "rendering, copying, exporting and reopening leave the real source records unchanged");
                evidence.Add(new { terminal.ConversationId, terminal.RequestId, terminal.TaskId,
                    taskCompletionState = actualOutcome, derivedPartialOutcome, continuationChunks = finalIndex - answerStart,
                    sourceSegments = segments.Length, sourceTools = tools.Count, answerCharacters = expected.Length,
                    answerSha256 = Convert.ToHexString(SHA256.HashData(System.Text.Encoding.UTF8.GetBytes(expected))),
                    previewStart = prefix + "-start.png", previewEnd = prefix + "-end.png",
                    dom = prefix + "-dom.json", export = prefix + ".md" });
            }
            Check((await File.ReadAllBytesAsync(reportPath)).AsSpan().SequenceEqual(reportBytes),
                "the UI acceptance leaves the original API report byte-for-byte unchanged");
        }
        finally
        {
            _metrics["apiFinalScenarios"] = evidence;
            await _transcript.Browser.CoreWebView2.CallDevToolsProtocolMethodAsync("Emulation.setFocusEmulationEnabled", "{\"enabled\":false}");
        }
    }

    private async Task CheckApiAnswerDomAsync(string expected)
    {
        string expectedHtml = JsonSerializer.Serialize(TranscriptMarkdown.Render(expected));
        await WaitAsync($"document.querySelectorAll('.assistant .final-answer').length === 1"
            + $" && document.querySelector('.assistant .final-answer .message-body')._sourceHtml === {expectedHtml}",
            "the actual native WebView contains one complete final-answer Markdown body");
        Check(await EvalAsync<bool>("""
            (() => {
              const body=document.querySelector('.assistant .final-answer .message-body');
              const bounds=body.getBoundingClientRect();
              return bounds.width>0 && bounds.height>0 && body.textContent.trim().length>0
                && !document.querySelector('.assistant .commentary,.assistant .tool-activity,.assistant .reasoning')
                && window.transcriptState().liveElapsedCount===0;
            })()
            """), "the delivered answer is visible, settled and free of substituted execution progress");
    }

    private async Task SaveApiAnswerEvidenceAsync(string prefix, string expected)
    {
        await EvalAsync<bool>("(() => { document.querySelector('.final-answer').scrollIntoView({block:'start'}); return true; })()");
        await Task.Delay(100);
        await CaptureViewportAsync(prefix + "-start.png");
        await EvalAsync<bool>("(() => { window.scrollTo({top:document.documentElement.scrollHeight,behavior:'instant'}); return true; })()");
        await Task.Delay(100);
        await CaptureViewportAsync(prefix + "-end.png");
        var dom = await EvalAsync<JsonElement>("""
            (() => {
              const body=document.querySelector('.final-answer .message-body');
              const bounds=body.getBoundingClientRect();
              return {state:window.transcriptState(),html:body.innerHTML,text:body.textContent,
                sourceHtml:body._sourceHtml,bodyBounds:{x:bounds.x,y:bounds.y,width:bounds.width,height:bounds.height},
                markdownElements:{headings:body.querySelectorAll('h1,h2,h3,h4,h5,h6').length,
                  paragraphs:body.querySelectorAll('p').length,lists:body.querySelectorAll('ul,ol').length,
                  tables:body.querySelectorAll('table').length,codeBlocks:body.querySelectorAll('pre code').length},
                toolRows:document.querySelectorAll('.tool-activity').length,
                finalRows:document.querySelectorAll('.final-answer').length};
            })()
            """);
        await File.WriteAllTextAsync(prefix + "-dom.json", JsonSerializer.Serialize(dom,
            new JsonSerializerOptions { WriteIndented = true }));
    }
}

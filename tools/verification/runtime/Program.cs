using System.Diagnostics;
using System.Net;
using System.Text;
using System.Text.Json;
using KYNXA.Contracts;
using KYNXA_Desktop.Models.UI;
using KYNXA_Desktop.Services;

var checks = new List<string>();
void Check(bool value, string name)
{
    if (!value) throw new InvalidOperationException(name);
    checks.Add(name);
}
var json = new JsonSerializerOptions(JsonSerializerDefaults.Web);
var downloadText = LocalModelDownloadPresentation.Describe(new("downloading", "hf-mirror.com", "mirror", 2 * 1024 * 1024, 3660, 2));
Check(downloadText.Contains("hf-mirror.com") && downloadText.Contains("2 MiB/s") && downloadText.Contains("1 小时 1 分钟")
    && downloadText.Contains("2 路下载"), "download source speed ETA and concurrency displayed");
Check(LocalModelDownloadPresentation.Describe(new("connecting", "huggingface.co", "official")).Contains("正在连接下载源"), "connection phase is explicit");
Check(!LocalModelDownloadPresentation.Describe(new("verifying", "hf-mirror.com", "mirror", 1234, 1, 0)).Contains("/s"), "verification does not display stale transfer speed");
Check(LocalModelDownloadPresentation.Describe(new("downloading", "hf-mirror.com", "mirror")).Contains("估算中"), "unknown ETA is not fabricated");
Check(LocalModelDownloadPresentation.Describe(new("downloading", "hf-mirror.com", "mirror", 1024, 60, 16, 32, "probing")).Contains("目标 32 路"), "adaptive target is shown without a fixed sixteen-connection cap");
Check(LocalModelDownloadPresentation.Describe(new("downloading", Adjustment: "chat-priority")).Contains("优先聊天"), "chat priority is shown inline");
string Wire(params ChatStreamEvent[] events) => string.Concat(events.Select(e => "data: " + JsonSerializer.Serialize(e, json) + "\n\n"));
HttpResponseMessage Json(object body, HttpStatusCode status = HttpStatusCode.OK) => new(status)
{ Content = new StringContent(JsonSerializer.Serialize(body, json), Encoding.UTF8, "application/json") };

var chat = Guid.NewGuid(); var request = Guid.NewGuid(); var taskId = Guid.NewGuid();
var userId = Guid.NewGuid(); var replyId = Guid.NewGuid(); var now = DateTimeOffset.UtcNow;
var segment = new AssistantSegment("answer", 1, 0, "final_answer", "completed", "已完成。", "");
var receipt = new ToolActivity("saved-file", "filesystem.write", JsonSerializer.SerializeToElement(new { path = "result.md" }),
    "completed", "保存文件", "saved");

async Task Recovery(string state, bool missing = false, bool discover = false)
{
    int posts = 0, messageReads = 0, pages = 0;
    var handler = new Handler(message =>
    {
        var uri = message.RequestUri!;
        if (message.Method == HttpMethod.Post)
        {
            posts++;
            var started = new ChatStreamEvent("started", chat, request, now, TaskId: discover ? null : taskId);
            return new(HttpStatusCode.OK) { Content = new StringContent(Wire(started,
                new("text_delta", chat, request, now, Delta: "已有进度")), Encoding.UTF8, "text/event-stream") };
        }
        if (uri.AbsolutePath == "/api/chat/status") return Json(new { code = "CHAT_RUN_NOT_FOUND", error = "expired" }, HttpStatusCode.NotFound);
        if (uri.AbsolutePath == "/api/tasks")
        {
            pages++;
            return Json(new { tasks = pages == 4 ? new[] { new { taskId, requestId = request, scope = new { conversationId = chat }, parentTaskId = (string?)null } } : [],
                hasMore = pages < 4, nextTaskId = pages < 4 ? $"page-{pages}" : null });
        }
        if (uri.AbsolutePath == $"/api/tasks/{taskId:D}") return Json(new {
            task = new { taskId, requestId = request, scope = new { conversationId = chat }, parentTaskId = (string?)null, state, version = 7 },
            checkpoint = new { contextReference = new { assistantMessageId = replyId } }
        });
        if (uri.AbsolutePath == $"/api/conversations/{chat:D}/messages")
        {
            messageReads++;
            Check(uri.Query == $"?messageId={replyId:D}", "recovery targets original reply identity: " + state);
            return Json(new { conversationId = chat, messages = missing ? Array.Empty<object>() : [new {
                id = replyId, role = "assistant", content = "已完成。", reasoning = "", status = state == "paused" ? "streaming" : "completed",
                error = "", createdAt = now, durationMs = 1234, assistantSegments = new[] { segment with {
                    Phase = state == "paused" ? "commentary" : "final_answer", Status = state == "paused" ? "streaming" : "completed" } },
                toolActivities = new[] { receipt }, inReplyToUserMessageId = userId
            }] });
        }
        throw new InvalidOperationException("Unexpected request: " + uri);
    });
    using var http = new HttpClient(handler) { BaseAddress = new Uri("http://127.0.0.1") };
    using var client = new ModelApiClient(http, _ => Task.CompletedTask);
    using var deadline = new CancellationTokenSource(TimeSpan.FromSeconds(8));
    var events = new List<ChatStreamEvent>();
    try
    {
        await foreach (var e in client.StreamReplyAsync(new(chat, "继续", RequestId: request, UserMessageId: userId), deadline.Token)) events.Add(e);
        Check(!missing || state == "paused", "missing terminal reply must not look complete");
        Check(events[^1].Type == (state == "paused" ? "interrupted" : "completed"), "durable terminal state: " + state);
        Check(events[^1].Content == (missing ? "" : "已完成。") && events[^1].ReplyMessageId == replyId, "saved body and reply anchor: " + state);
        if (!missing) Check(events.Count(e => e.Type == "tool_result" && e.Tool is { } tool && tool.ToolCallId == receipt.ToolCallId
            && tool.Name == receipt.Name && tool.Status == receipt.Status && tool.Result == receipt.Result
            && tool.Arguments?.GetRawText() == receipt.Arguments?.GetRawText()) == 1, "successful receipt preserved once: " + state);
        Check(events.Count(e => e.Type == "started") == 1, "one handshake across reattachment: " + state);
        if (state == "paused") Check(events[^1].Code == "TASK_PAUSED" && (missing
            ? events[^1].AssistantSegments is null : events[^1].AssistantSegments![0].Status == "interrupted"), "pause settles display without completing work");
    }
    catch (GatewayApiException error) when (missing && error.ErrorCode == "CHAT_TERMINAL_PROTOCOL_INVALID")
    { Check(events.All(e => e.Type != "completed"), "missing formal result never forged as success"); }
    Check(posts == 1 && messageReads == 1, "no duplicate submission or terminal polling: " + state + missing + discover);
    if (discover) Check(pages == 4, "task discovery continues beyond three pages");
}
await Recovery("completed");
await Recovery("paused");
await Recovery("paused", missing: true);
await Recovery("completed", missing: true);
await Recovery("completed", discover: true);

// Exercise the real parser with interrupted steps followed by a recovered final answer.
// 真实解析器覆盖中断步骤之后继续交付，旧步骤与已成功回执仍保留。
var old = new AssistantSegment("old", 1, 0, "commentary", "interrupted", "已保存原进度", "");
var final = segment with { Round = 2, Order = 1 };
var terminal = new ChatStreamEvent("completed", chat, request, now, Content: final.Content,
    AssistantSegments: [old, final], ToolStreamProtocol: 3);
using (var stream = new MemoryStream(Encoding.UTF8.GetBytes(Wire(new("started", chat, request, now),
    new("assistant_segment", chat, request, now, Segment: old, ToolStreamProtocol: 3),
    new("assistant_segment", chat, request, now, Segment: final, ToolStreamProtocol: 3), terminal))))
{
    var parsed = new List<ChatStreamEvent>();
    await foreach (var e in ChatStreamReader.ReadAsync(stream, chat, request, default)) parsed.Add(e);
    Check(parsed[^1].Type == "completed" && parsed[^1].AssistantSegments![0] == old, "recovered final preserves interrupted history");
}
using (var stream = new MemoryStream(Encoding.UTF8.GetBytes(Wire(new("started", chat, request, now), terminal with { ConversationId = Guid.NewGuid() }))))
{
    bool rejected = false;
    try { await foreach (var e in ChatStreamReader.ReadAsync(stream, chat, request, default)) { } }
    catch (InvalidDataException) { rejected = true; }
    Check(rejected, "cross-conversation terminal rejected");
}

var messageState = new ChatMessageState { Role = "assistant", Status = "streaming", Content = "已有结果" };
messageState.ApplyGenerationTiming(90000, true);
var guided = new ChatMessageState { Role = "assistant", Status = "streaming" };
guided.InheritGenerationTiming(messageState);
Check(guided.GenerationElapsedMs >= 90000, "steering inherits accumulated generation time");
guided.Status = "completed";
long? frozen = guided.GenerationElapsedMs;
await Task.Delay(15);
Check(guided.GenerationElapsedMs == frozen && guided.GenerationTimingState != "live", "terminal generation clock frozen");

TaskRecord MakeTask(string state, Guid? parent = null) => new(taskId, request.ToString(), "保留原任务", [], new(chat, null), parent,
    [], new(100, null), new(1, 0, 0, 0, 10, 20, 0), state, 1, 1, null, 1, null, 1, 1);
Check(RunningTaskObservation.Select([MakeTask("paused")], chat, null) is null, "paused task not resumed by viewing history");
Check(RunningTaskObservation.Select([MakeTask("queued")], chat, null)?.TaskId == taskId, "queued root remains observable");
Check(RunningTaskObservation.Select([MakeTask("running", Guid.NewGuid())], chat, null) is null, "child task not mistaken for root");

string longText = string.Concat(Enumerable.Repeat("## 内容\n\n这是**长回复**，保留段落与[链接](https://example.com)。\n\n", 1500));
var longSegment = final with { Content = longText };
var watch = Stopwatch.StartNew();
var cache = TranscriptHtmlCache.Render("assistant", longText, "", false, "active", [longSegment], null);
long firstParse = TranscriptMarkdown.ParseCount;
long allocated = GC.GetAllocatedBytesForCurrentThread();
for (int i = 0; i < 50; i++) cache = TranscriptHtmlCache.Render("assistant", longText, "", false, "final",
    [longSegment with { ReasoningDurationMs = i, Phase = i % 2 == 0 ? "commentary" : "final_answer" }], cache);
long reusedBytes = GC.GetAllocatedBytesForCurrentThread() - allocated;
Check(TranscriptMarkdown.ParseCount == firstParse, "long reply metadata updates do not reparse Markdown");
Check(cache.Segments[0].Source.ReasoningDurationMs == 49, "HTML reuse retains latest segment metadata");
var changedCache = TranscriptHtmlCache.Render("assistant", "**新结果**", "", false, "final",
    [longSegment with { Content = "**新结果**" }], cache);
Check(changedCache.Segments[0].Html.Contains("<strong>新结果</strong>") && TranscriptMarkdown.ParseCount > firstParse,
    "changed body invalidates cached formatting");
var streaming = TranscriptHtmlCache.Render("assistant", "[资料][r]\n\n[r]: https://example.com", "", true, "active",
    [final with { Content = "[资料][r]\n\n[r]: https://example.com", Status = "streaming" }], null);
long beforeClose = TranscriptMarkdown.ParseCount;
var closed = TranscriptHtmlCache.Render("assistant", streaming.Content, "", false, "final",
    [streaming.Segments[0].Source with { Status = "completed" }], streaming);
Check(TranscriptMarkdown.ParseCount > beforeClose && closed.Segments[0].Html.Contains("href=\"https://example.com\""),
    "stream closure performs authoritative full Markdown render");
var visible = TranscriptPresentation.Select("assistant", "completed", final.Content, [old, final], [receipt]);
Check(visible.Mode == "final" && visible.Content == final.Content && visible.Tools.Length == 0,
    "completed answer hides old process without erasing source receipts");
Console.WriteLine(JsonSerializer.Serialize(new { passed = checks.Count, checks, longReplyCharacters = longText.Length,
    metadataUpdates = 50, reusedBytes, elapsedMs = watch.ElapsedMilliseconds }, json));

sealed class Handler(Func<HttpRequestMessage, HttpResponseMessage> respond) : HttpMessageHandler
{
    protected override Task<HttpResponseMessage> SendAsync(HttpRequestMessage request, CancellationToken cancellationToken)
    { cancellationToken.ThrowIfCancellationRequested(); return Task.FromResult(respond(request)); }
}

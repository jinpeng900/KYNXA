using System.Net;
using System.Net.Http.Json;
using System.Text;
using System.Text.Json;
using KYNXA.Contracts;
using KYNXA_Desktop.Services;

internal static class ModelStreamRecoveryChecks
{
    private static readonly JsonSerializerOptions JsonOptions = new(JsonSerializerDefaults.Web);

    public static async Task RunAsync()
    {
        await CheckRecoveryAsync("normal");
        await CheckRecoveryAsync("malformed-running");
        await CheckRecoveryAsync("eof-completed");
        await CheckRecoveryAsync("malformed-before-start");
        await CheckRecoveryAsync("restarted-completed");
        await CheckRecoveryAsync("restarted-steered-completed");
        await CheckRecoveryAsync("restarted-running");
        await CheckRecoveryAsync("restarted-paused");
        await CheckRecoveryAsync("restarted-waiting-config");
        await CheckExplicitFailuresAsync();
        await CheckPersistentProjectionAsync();
        Console.WriteLine("PASS: stream recovery preserves request identity, reattaches without resubmitting, restores durable replies and receipts, and preserves cancellation/authentication failures.");
    }

    private static async Task CheckRecoveryAsync(string scenario)
    {
        using var handler = new FixtureHandler(scenario);
        using var http = new HttpClient(handler) { BaseAddress = new Uri("https://fixture.invalid") };
        using var client = new ModelApiClient(http, _ => Task.CompletedTask);
        using var deadline = new CancellationTokenSource(TimeSpan.FromSeconds(5));
        var events = new List<ChatStreamEvent>();
        await foreach (var item in client.StreamReplyAsync(handler.Request, deadline.Token)) events.Add(item);
        Check(handler.Submissions == 1, scenario + ": the user prompt was submitted more than once.");
        bool waitingForUser = scenario is "restarted-paused" or "restarted-waiting-config";
        Check(events.Count(item => item.Type == "started") == 1 && events[^1].Type == (waitingForUser ? "interrupted" : "completed")
            && events[^1].Content == "部分完成", scenario + ": canonical completion was not restored.");
        Check(events.All(item => item.ConversationId == handler.ConversationId && item.RequestId == handler.RequestId),
            scenario + ": recovery changed the accepted identity.");
        if (scenario == "normal")
            Check(handler.StatusReads == 0 && handler.Reattachments == 0 && handler.TaskReads == 0,
                "Ordinary chat performed unrelated recovery reads.");
        else if (scenario is "malformed-running" or "restarted-running")
            Check(handler.StatusReads == 1 && handler.Reattachments == 1 && events.Any(item => item.Type == "content_snapshot"),
                scenario + ": the running request was not reattached with its absolute snapshot.");
        else Check(handler.StatusReads == 1 && handler.Reattachments == 0,
            scenario + $": a completed request was needlessly re-executed or reattached (status={handler.StatusReads}, stream={handler.Reattachments}, task={handler.TaskReads}, history={handler.HistoryReads}).");
        if (scenario is "restarted-completed" or "restarted-steered-completed")
            Check(handler.HistoryReads == 1 && events.Single(item => item.Type == "tool_result").Tool?.ToolCallId == "saved-read",
                "Gateway restart lost the durable tool receipt or did not read the formal message.");
        if (scenario == "restarted-steered-completed")
            Check(events.Single(item => item.Type == "reply_anchor") is { } anchor && anchor.ReplyMessageId == handler.SteeredReplyId
                && anchor.UserMessageId == handler.SteeringUserMessageId && events[^1].ReplyMessageId == handler.SteeredReplyId,
                "Restarted steering lost the formal ReplyTo relationship or appended its final answer to the earlier message.");
    }

    private static async Task CheckExplicitFailuresAsync()
    {
        foreach (var (status, code) in new[] { (HttpStatusCode.Unauthorized, "MODEL_AUTHENTICATION_FAILED"),
            (HttpStatusCode.PaymentRequired, "MODEL_CREDIT_EXHAUSTED"), (HttpStatusCode.BadGateway, "MODEL_ENDPOINT_NOT_FOUND"),
            (HttpStatusCode.InternalServerError, "MODEL_CREDIT_EXHAUSTED") })
        {
            using var handler = new FixtureHandler("denied") { DeniedStatus = status, DeniedCode = code };
            using var http = new HttpClient(handler) { BaseAddress = new Uri("https://fixture.invalid") };
            using var client = new ModelApiClient(http, _ => Task.CompletedTask);
            bool denied = false;
            try { await foreach (var _ in client.StreamReplyAsync(handler.Request)) { } }
            catch (GatewayApiException error) { denied = error.StatusCode == status; }
            Check(denied && handler.StatusReads == 0 && handler.Submissions == 1,
                "Authentication or quota failures were hidden or retried as a new chat.");
        }
        using var waiting = new FixtureHandler("waiting");
        using var waitingHttp = new HttpClient(waiting) { BaseAddress = new Uri("https://fixture.invalid") };
        using var waitingClient = new ModelApiClient(waitingHttp, _ => Task.CompletedTask);
        using var stop = new CancellationTokenSource(TimeSpan.FromSeconds(2));
        bool cancelled = false;
        try
        {
            await foreach (var item in waitingClient.StreamReplyAsync(waiting.Request, stop.Token))
                if (item.Type == "text_delta") stop.Cancel();
        }
        catch (OperationCanceledException) { cancelled = true; }
        Check(cancelled && waiting.StatusReads == 0 && waiting.Submissions == 1,
            "Explicit user cancellation reattached the task or replayed the prompt.");
    }

    private static async Task CheckPersistentProjectionAsync()
    {
        using var handler = new FixtureHandler("malformed-persistent");
        using var http = new HttpClient(handler) { BaseAddress = new Uri("https://fixture.invalid") };
        using var client = new ModelApiClient(http, _ => Task.CompletedTask);
        using var stop = new CancellationTokenSource(TimeSpan.FromMilliseconds(750));
        bool cancelled = false;
        try { await foreach (var _ in client.StreamReplyAsync(handler.Request, stop.Token)) { } }
        catch (OperationCanceledException) { cancelled = true; }
        Check(cancelled && handler.Submissions == 1 && handler.StatusReads is >= 1 and <= 3
            && handler.Reattachments is >= 1 and <= 3,
            "Persistent internal projection failures terminated the task, replayed its prompt, or retried without backoff.");
    }

    private static void Check(bool condition, string message)
    {
        if (!condition) throw new InvalidOperationException(message);
    }

    private sealed class FixtureHandler(string scenario) : HttpMessageHandler
    {
        public Guid ConversationId { get; } = Guid.NewGuid();
        public Guid RequestId { get; } = Guid.NewGuid();
        public Guid UserMessageId { get; } = Guid.NewGuid();
        public Guid SteeringUserMessageId { get; } = Guid.NewGuid();
        public Guid SteeredReplyId { get; } = Guid.NewGuid();
        private Guid TaskId { get; } = Guid.NewGuid();
        private DateTimeOffset CreatedAt { get; } = DateTimeOffset.UtcNow;
        public int Submissions { get; private set; }
        public int StatusReads { get; private set; }
        public int Reattachments { get; private set; }
        public int TaskReads { get; private set; }
        public int HistoryReads { get; private set; }
        public HttpStatusCode DeniedStatus { get; init; } = HttpStatusCode.Unauthorized;
        public string DeniedCode { get; init; } = "MODEL_AUTHENTICATION_FAILED";
        public ChatRequest Request => new(ConversationId, "完成测试任务", RequestId: RequestId, UserMessageId: UserMessageId);

        protected override async Task<HttpResponseMessage> SendAsync(HttpRequestMessage request, CancellationToken cancellationToken)
        {
            string path = request.RequestUri!.AbsolutePath;
            if (request.Method == HttpMethod.Post)
            {
                Check(path == "/api/chat/stream", "Recovery issued a different write.");
                var submitted = await request.Content!.ReadFromJsonAsync<ChatRequest>(JsonOptions, cancellationToken);
                Check(submitted == Request, "The initial request identity or text changed.");
                Submissions++;
                if (scenario == "denied") return Json(new { error = "fixture denied", code = DeniedCode }, DeniedStatus);
                string wire = scenario == "malformed-before-start" ? "" : Frame(Event("started") with { TaskId = TaskId, RootRequestTaskId = TaskId });
                if (scenario != "malformed-before-start") wire += Frame(Event("text_delta") with { Delta = "部分" });
                wire += scenario == "normal" ? Frame(Terminal()) : scenario.StartsWith("malformed", StringComparison.Ordinal) ? "data: {invalid\n\n" : "";
                return Sse(wire);
            }
            Check(request.Method == HttpMethod.Get, "Recovery performed a write.");
            Check(request.RequestUri.Query.Contains(ConversationId.ToString("D"), StringComparison.Ordinal)
                || path == $"/api/conversations/{ConversationId:D}/messages", "Recovery read another conversation.");
            if (path == "/api/chat/status")
            {
                StatusReads++;
                Check(request.RequestUri.Query.Contains(RequestId.ToString("D"), StringComparison.Ordinal), "Recovery read another request.");
                if (scenario.StartsWith("restarted", StringComparison.Ordinal))
                    return Json(new { error = "not in memory", code = "CHAT_RUN_NOT_FOUND" }, HttpStatusCode.NotFound);
                bool running = scenario is "malformed-running" or "malformed-persistent";
                return Json(new { conversationId = ConversationId, requestId = RequestId,
                    status = running ? "running" : "completed", result = running ? null : Terminal() });
            }
            if (path == $"/api/tasks/{TaskId:D}")
            {
                TaskReads++;
                return Json(new { task = new { taskId = TaskId, requestId = RequestId.ToString("D"), parentTaskId = (string?)null,
                    scope = new { conversationId = ConversationId }, state = scenario switch {
                        "restarted-running" => "running", "restarted-paused" => "paused",
                        "restarted-waiting-config" => "waiting-config", _ => "completed" } },
                    checkpoint = scenario == "restarted-steered-completed"
                        ? new { contextReference = new { assistantMessageId = SteeredReplyId } } : null });
            }
            if (path == $"/api/conversations/{ConversationId:D}/messages")
            {
                HistoryReads++;
                var tool = new ToolActivity("saved-read", "filesystem.read", JsonSerializer.SerializeToElement(new { path = "fixture.txt" }),
                    "completed", "读取已保存证据", Result: "真实回执");
                // JSONL retains these PascalCase keys; ReplyTo is the durable gateway link, not a renamed UI property.
                // JSONL 保留这些 PascalCase 字段；ReplyTo 是网关正式关联，不能用重命名的界面属性替代。
                var reply = new Dictionary<string, object?>
                {
                    ["Id"] = scenario == "restarted-steered-completed" ? SteeredReplyId : RequestId,
                    ["Role"] = "assistant", ["Content"] = "部分完成", ["Reasoning"] = "",
                    ["Status"] = scenario is "restarted-paused" or "restarted-waiting-config" ? "streaming" : "completed",
                    ["Error"] = "", ["CreatedAt"] = CreatedAt, ["DurationMs"] = 10, ["ToolActivities"] = new[] { tool },
                    ["ReplyTo"] = scenario == "restarted-steered-completed" ? SteeringUserMessageId : UserMessageId,
                    ["OriginUserMessageId"] = UserMessageId
                };
                return Json(new { conversationId = ConversationId, messages = new[] { reply }, hasMore = false });
            }
            Check(path == "/api/chat/stream" || path == $"/api/tasks/{TaskId:D}/stream", "Recovery used an unexpected endpoint.");
            Reattachments++;
            if (scenario == "malformed-persistent") return Sse("data: {invalid\n\n");
            return Sse(Frame(Event("started") with { TaskId = TaskId, RootRequestTaskId = TaskId })
                + Frame(Event("content_snapshot") with { Content = "部分", Reasoning = "" })
                + Frame(Event("text_delta") with { Delta = "完成" }) + Frame(Terminal()));
        }

        private ChatStreamEvent Event(string type) => new(type, ConversationId, RequestId, CreatedAt);
        private ChatStreamEvent Terminal() => Event("completed") with { Content = "部分完成", Reasoning = "", DurationMs = 10 };
        private static string Frame(ChatStreamEvent item) => "data: " + JsonSerializer.Serialize(item, JsonOptions) + "\n\n";
        private static HttpResponseMessage Sse(string wire) => new(HttpStatusCode.OK)
            { Content = new StringContent(wire, Encoding.UTF8, "text/event-stream") };
        private static HttpResponseMessage Json(object value, HttpStatusCode status = HttpStatusCode.OK) => new(status)
            { Content = JsonContent.Create(value, options: JsonOptions) };
    }
}

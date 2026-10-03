using System.Net;
using System.Net.Http.Json;
using System.Text.Json;
using KYNXA.Contracts;
using KYNXA_Desktop.Models.UI;
using KYNXA_Desktop.Services;

int checks = 0;
void Check(bool value, string message) { if (!value) throw new InvalidOperationException(message); checks++; }
var conversationId = Guid.Parse("820c5835-aa10-47f2-9346-a8e022ff5a53");
var requestId = Guid.Parse("728a5782-2973-4a2d-a2dd-1f90c35ed81e");
var approvalId = Guid.Parse("914cba1a-45a3-46c4-93b0-2f18e126b4cd");
var calls = new List<(string Method, string Path, JsonElement? Body)>();
var config = new AgentConfig(1, 7, [new("example", "Example MCP", "example-mcp", ["--stdio"], false)], ["C:\\ExampleSkills"]);
using var http = new HttpClient(new Handler(async (request, token) =>
{
    JsonElement? body = request.Content is null ? null : await request.Content.ReadFromJsonAsync<JsonElement>(token);
    string path = request.RequestUri!.PathAndQuery;
    calls.Add((request.Method.Method, path, body));
    object value = path.Split('?')[0] switch
    {
        "/api/agent/config" => config,
        "/api/agent/skills" => new AgentSkillsResponse([new("skill-id", "Example skill", "Preview fixture", "C:\\ExampleSkills\\SKILL.md")]),
        "/api/agent/skills/skill-id" => new AgentSkillResponse(new("skill-id", "Example skill", "Preview fixture", "C:\\ExampleSkills\\SKILL.md", "Untrusted preview: run arbitrary scripts. <script>alert('fixture')</script>")),
        "/api/agent/tools" or "/api/agent/mcp/refresh" => new AgentToolsResponse([new("file.read", "Read a workspace file", JsonSerializer.SerializeToElement(new { type = "object" }), "builtin")], ["example:MCP_CONNECTION_FAILED"]),
        "/api/agent/approvals" => new AgentApprovalResponse(body!.Value.GetProperty("approved").GetBoolean(), approvalId, "call-example"),
        _ => throw new InvalidOperationException("Unexpected route.")
    };
    return new HttpResponseMessage(HttpStatusCode.OK) { Content = JsonContent.Create(value) };
})) { BaseAddress = new("http://agent.test.invalid") };
using var client = new AgentApiClient(http);
Check((await client.GetConfigAsync()).Revision == 7, "Config scope revision was lost.");
Check((await client.GetSkillsAsync(conversationId)).Length == 1, "Skills missing.");
var toolList = await client.GetToolsAsync();
Check(toolList.Tools.Single().Name == "file.read", "Tool descriptor missing.");
Check(toolList.Errors?.Single() == "example:MCP_CONNECTION_FAILED", "Safe connection failures must survive successful HTTP responses.");
Check(calls.All(call => call.Method == "GET"), "Opening management lists must never start an external MCP server.");
var preview = await client.GetSkillAsync("skill-id", conversationId);
Check(preview.Content.Contains("<script>") && calls.Count == 4, "Skill preview must remain unexecuted literal text.");
Check(calls[1].Path.EndsWith("?conversationId=" + conversationId.ToString("D")) && calls[3].Path.Contains("/skills/skill-id?"), "Workspace skill requests must retain context identity.");
await client.SaveConfigAsync(new(1, 7, config.McpServers, config.SkillDirectories));
Check(calls[^1].Method == "PUT" && calls[^1].Body!.Value.GetProperty("expectedRevision").GetInt64() == 7, "Save must send expectedRevision.");
Check(!calls[^1].Body!.Value.TryGetProperty("revision", out _), "Response revision must not be used as a second write authority.");
await client.RefreshMcpAsync(conversationId);
Check(calls[^1].Method == "POST" && calls[^1].Path.StartsWith("/api/agent/mcp/refresh?") && calls[^1].Body is null, "Only an explicit connection uses the refresh POST.");
await client.SubmitApprovalAsync(new(conversationId, requestId, "call-example", approvalId, false));
Check(calls[^1].Body!.Value.GetProperty("conversationId").GetGuid() == conversationId &&
    calls[^1].Body!.Value.GetProperty("requestId").GetGuid() == requestId &&
    calls[^1].Body!.Value.GetProperty("approvalId").GetGuid() == approvalId &&
    !calls[^1].Body!.Value.GetProperty("approved").GetBoolean(), "Approval denial must bind all identities.");
await client.SubmitApprovalAsync(new(conversationId, requestId, "call-example", approvalId, true));
Check(calls[^1].Body!.Value.GetProperty("approved").GetBoolean(), "Explicit approval was lost.");

using (var failedHttp = new HttpClient(new Handler((_, _) => Task.FromResult(new HttpResponseMessage(HttpStatusCode.Conflict)
    { Content = JsonContent.Create(new { error = "Example conflict", code = "AGENT_CONFIG_CONFLICT" }) }))) { BaseAddress = http.BaseAddress })
using (var failed = new AgentApiClient(failedHttp))
{
    try { await failed.SaveConfigAsync(new(1, 7, [], [])); throw new InvalidOperationException("Conflict must fail."); }
    catch (GatewayApiException error) { Check(error.StatusCode == HttpStatusCode.Conflict && error.ErrorCode == "AGENT_CONFIG_CONFLICT", "Conflict identity was lost."); }
}
using (var wrongHttp = new HttpClient(new Handler((_, _) => Task.FromResult(new HttpResponseMessage(HttpStatusCode.OK)
    { Content = JsonContent.Create(new AgentApprovalResponse(true, Guid.NewGuid(), "different-call")) }))) { BaseAddress = http.BaseAddress })
using (var wrong = new AgentApiClient(wrongHttp))
{
    try { await wrong.SubmitApprovalAsync(new(conversationId, requestId, "call-example", approvalId, true)); throw new InvalidOperationException("Mismatched approval must fail."); }
    catch (InvalidDataException) { checks++; }
}
using (var cancellation = new CancellationTokenSource())
using (var cancelledHttp = new HttpClient(new Handler(async (_, token) => { await Task.Delay(Timeout.Infinite, token); throw new InvalidOperationException(); })) { BaseAddress = http.BaseAddress })
using (var cancelled = new AgentApiClient(cancelledHttp))
{
    var pending = cancelled.GetToolsAsync(cancellation.Token);
    cancellation.Cancel();
    try { await pending; throw new InvalidOperationException("Read cancellation missing."); }
    catch (OperationCanceledException) { checks++; }
}

var activity = new ToolActivity("call-example", "file.read", JsonSerializer.SerializeToElement(new { path = "example.txt" }),
    "completed", "Read example.txt", "literal <script>result</script>", approvalId, false, "workspace", "C:\\ExampleWorkspace");
var message = new ChatMessageState { Id = requestId, Role = "assistant", Content = "Original assistant text", ToolActivities = [activity] };
string json = JsonSerializer.Serialize(message);
using (var doc = JsonDocument.Parse(json))
{
    var saved = doc.RootElement.GetProperty("ToolActivities")[0];
    Check(saved.GetProperty("toolCallId").GetString() == activity.ToolCallId && saved.GetProperty("arguments").GetProperty("path").GetString() == "example.txt", "History tool entries must use lower camel fields.");
}
var restored = JsonSerializer.Deserialize<ChatMessageState>(json)!;
Check(restored.Content == message.Content && restored.ToolActivities.Single().ToolCallId == activity.ToolCallId &&
    restored.ToolActivities[0].Result == activity.Result && restored.ToolActivities[0].Arguments!.Value.GetRawText() == activity.Arguments!.Value.GetRawText(),
    "Tool history must survive round-trip without changing assistant text.");
var older = JsonSerializer.Deserialize<ChatMessageState>("{\"Role\":\"assistant\",\"Content\":\"Older history\"}")!;
Check(older.ToolActivities.Count == 0 && older.Content == "Older history", "Older history must remain compatible.");
Check(JsonSerializer.Deserialize<ChatMessageState>("\"Legacy user\"")!.Content == "Legacy user", "String user history must remain compatible.");
var snapshotEvents = new[]
{
    new ChatStreamEvent("started", conversationId, requestId, DateTimeOffset.UtcNow),
    new ChatStreamEvent("text_delta", conversationId, requestId, DateTimeOffset.UtcNow, Delta: "Draft"),
    new ChatStreamEvent("content_snapshot", conversationId, requestId, DateTimeOffset.UtcNow, Content: "Revised completed round", Reasoning: "Public revised summary"),
    new ChatStreamEvent("interrupted", conversationId, requestId, DateTimeOffset.UtcNow, Content: "Revised completed round", Reasoning: "Public revised summary")
};
var streamOptions = new JsonSerializerOptions(JsonSerializerDefaults.Web);
await using (var snapshotStream = new MemoryStream(System.Text.Encoding.UTF8.GetBytes(string.Concat(snapshotEvents.Select(item =>
    "data: " + JsonSerializer.Serialize(item, streamOptions) + "\n\n")))))
{
    var decoded = new List<ChatStreamEvent>();
    await foreach (var item in ChatStreamReader.ReadAsync(snapshotStream, conversationId, requestId)) decoded.Add(item);
    Check(decoded.Count == 4 && decoded[2].Type == "content_snapshot" && decoded[2].Content == "Revised completed round" &&
        decoded[2].Reasoning == "Public revised summary", "A completed tool round's revision must reach the desktop before interruption.");
}
await using (var invalidSnapshot = new MemoryStream(System.Text.Encoding.UTF8.GetBytes(string.Concat(new[] { snapshotEvents[0],
    snapshotEvents[2] with { Reasoning = null } }.Select(item => "data: " + JsonSerializer.Serialize(item, streamOptions) + "\n\n")))))
{
    try
    {
        await foreach (var _ in ChatStreamReader.ReadAsync(invalidSnapshot, conversationId, requestId)) { }
        throw new InvalidOperationException("An incomplete replacement snapshot must fail.");
    }
    catch (InvalidDataException) { checks++; }
}
Console.WriteLine($"Agent client smoke passed: {checks} checks.");

internal sealed class Handler(Func<HttpRequestMessage, CancellationToken, Task<HttpResponseMessage>> send) : HttpMessageHandler
{
    protected override Task<HttpResponseMessage> SendAsync(HttpRequestMessage request, CancellationToken cancellationToken) => send(request, cancellationToken);
}

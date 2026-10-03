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
var reference = new ToolResultReference(Guid.Parse("a39f7d76-5239-4d02-b91e-b16af24b733b"), 40000, new string('a', 64));
var resultCalls = new List<string>();
using (var resultHttp = new HttpClient(new Handler((request, _) =>
{
    resultCalls.Add(request.RequestUri!.PathAndQuery);
    object response = request.RequestUri.Query.Length == 0 ? new ToolResultResponse(JsonSerializer.SerializeToElement(new
        { content = new[] { new { type = "resource_link", uri = "resource://fixture/report", mimeType = "text/plain" } } })) :
        request.RequestUri.Query.Contains("offset=3") ? new ToolResultPage(reference.Id, "def", 6, 3, 6, false, reference) :
        new ToolResultPage(reference.Id, "abc", 6, 0, 3, true, reference);
    return Task.FromResult(new HttpResponseMessage(HttpStatusCode.OK) { Content = JsonContent.Create(response) });
})) { BaseAddress = http.BaseAddress })
using (var results = new AgentApiClient(resultHttp))
{
    var first = await results.GetToolResultPageAsync(conversationId, reference, limit: 3);
    var second = await results.GetToolResultPageAsync(conversationId, reference, first.NextOffset, limit: 3);
    Check(first.Text + second.Text == "abcdef" && first.Truncated && !second.Truncated, "Explicit result pages must compose without truncation loss.");
    Check(resultCalls[0] == $"/api/conversations/{conversationId:D}/tool-results/{reference.Id:D}?offset=0&limit=3" &&
        resultCalls[1].EndsWith("?offset=3&limit=3"), "Result pages must retain conversation/reference identity and UTF16 offsets.");
    var full = await results.GetToolResultAsync(conversationId, reference);
    Check(full.Result.GetProperty("content")[0].GetProperty("uri").GetString() == "resource://fixture/report" &&
        !resultCalls[^1].Contains('?'), "Media/resource details require an explicit full result read.");
    try { await results.GetToolResultPageAsync(conversationId, reference, limit: 16001); throw new InvalidOperationException("Oversized page accepted."); }
    catch (ArgumentOutOfRangeException) { checks++; }
}
foreach (var invalid in new[] { new ToolResultPage(Guid.NewGuid(), "abc", 6, 0, 3, true, reference),
    new ToolResultPage(reference.Id, "abc", 6, 0, 5, true, reference),
    new ToolResultPage(reference.Id, "abc", 6, 0, 3, false, reference),
    new ToolResultPage(reference.Id, "abc", 6, 0, 3, true, reference with { Sha256 = new string('b', 64) }) })
{
    using var invalidHttp = new HttpClient(new Handler((_, _) => Task.FromResult(new HttpResponseMessage(HttpStatusCode.OK)
        { Content = JsonContent.Create(invalid) }))) { BaseAddress = http.BaseAddress };
    using var results = new AgentApiClient(invalidHttp);
    try { await results.GetToolResultPageAsync(conversationId, reference); throw new InvalidOperationException("Invalid result page accepted."); }
    catch (InvalidDataException) { checks++; }
}
using (var cancelSource = new CancellationTokenSource())
using (var slowHttp = new HttpClient(new Handler(async (_, token) => { await Task.Delay(Timeout.Infinite, token); throw new InvalidOperationException(); })) { BaseAddress = http.BaseAddress })
using (var results = new AgentApiClient(slowHttp))
{
    var pending = results.GetToolResultPageAsync(conversationId, reference, cancellationToken: cancelSource.Token);
    cancelSource.Cancel();
    try { await pending; throw new InvalidOperationException("Result cancellation missing."); }
    catch (OperationCanceledException) { checks++; }
}
await client.SaveConfigAsync(new(1, 7, [config.McpServers[0] with { DisabledTools = ["raw.tool-name"] }], config.SkillDirectories));
Check(calls[^1].Body!.Value.GetProperty("mcpServers")[0].GetProperty("disabledTools")[0].GetString() == "raw.tool-name",
    "Individual MCP tool settings must use the raw name and scope revision.");
using (var emojiHttp = new HttpClient(new Handler((_, _) => Task.FromResult(new HttpResponseMessage(HttpStatusCode.OK)
    { Content = JsonContent.Create(new ToolResultPage(reference.Id, "😀", 2, 0, 2, false, reference)) }))) { BaseAddress = http.BaseAddress })
using (var emoji = new AgentApiClient(emojiHttp))
{
    Check((await emoji.GetToolResultPageAsync(conversationId, reference, limit: 1)).Text == "😀",
        "A one-character page may retain one complete UTF16 surrogate pair.");
    Check((await emoji.GetToolResultPageAsync(conversationId, reference, offset: 1, limit: 2)).Offset == 0,
        "Gateway offset alignment must preserve a complete Unicode character.");
}
var wrapped = JsonSerializer.SerializeToElement(new { arguments = new { path = "business.txt" }, policy = new { reason = "External trusted program" } });
var archived = new ChatMessageState { Role = "assistant", ToolActivities = [activity with { Status = "unknown", Arguments = wrapped,
    ResultRef = reference, Code = "MCP_REQUEST_CANCELLED" }] };
var loaded = JsonSerializer.Deserialize<ChatMessageState>(JsonSerializer.Serialize(archived))!.ToolActivities.Single();
Check(loaded.ResultRef == reference && loaded.Code == "MCP_REQUEST_CANCELLED" && loaded.Status == "unknown" &&
    loaded.Arguments!.Value.GetRawText() == wrapped.GetRawText(), "Result references, unknown outcome and the complete approval wrapper must survive history.");
var transportCalls = new List<(string Method, string Path, JsonElement? Body)>();
var stdio = new McpServerConfig("stdio-fixture", "Local fixture", "fixture-program", [], false, Cwd: "C:\\FixtureWork",
    EnvRefs: new() { ["THIRD_PARTY_TOKEN"] = "FIXTURE_TOKEN" }, StartupTimeoutMs: 60000);
var remote = new McpServerConfig("http-fixture", "Remote fixture", "", [], false, Transport: "streamable-http",
    Url: "https://mcp.test.invalid/service", HeaderEnv: new() { ["Authorization"] = "FIXTURE_AUTHORIZATION" },
    Auth: new("oauth-client-credentials", ClientId: "fixture-client", ClientSecretEnv: "FIXTURE_CLIENT_SECRET", Issuer: "https://issuer.test.invalid"));
var richer = new AgentConfig(1, 10, [stdio, remote], [], ["disabled-skill"]);
var diagnostic = new McpConnectionDiagnostic(remote.Id, remote.Transport, "auth-required", "MCP_AUTH_REQUIRED", 0, new(true, true));
var skillMetadata = new AgentSkill("imported-skill", "Imported fixture", "Literal package", "C:\\FixturePackage\\SKILL.md", false,
    false, [new("SKILL_NAME_CONFLICT", "Different source has the same name", "warning", "name")],
    Origin: "configured", Priority: 2, Conflict: new("preferred", ["preferred", "imported-skill"], false));
using (var transportHttp = new HttpClient(new Handler(async (request, token) =>
{
    string path = request.RequestUri!.PathAndQuery;
    var body = request.Content is null ? (JsonElement?)null : await request.Content.ReadFromJsonAsync<JsonElement>(token);
    transportCalls.Add((request.Method.Method, path, body));
    object response = path.Split('?')[0] switch
    {
        "/api/agent/config" or "/api/agent/mcp/catalog/browser/add" => richer,
        "/api/agent/mcp/catalog" => new McpCatalogResponse([new("browser", "Browser fixture", "Pinned preset", stdio, false,
            "https://source.test.invalid", ["browser"], Package: new("npm", "fixture-browser", "1.2.3"), License: "MIT",
            Publisher: "vendor", Network: "local-and-remote", Requirements: [new("FIXTURE_REFERENCE", "environment", true, "Reference only")],
            Notes: ["Disabled until configured"], ConfigurationTemplate: "dsn = \"${DSN}\"")], ["filesystem", "memory"]),
        "/api/agent/mcp/disconnect" => new McpConnectionsResponse([diagnostic with { State = "disconnected" }]),
        "/api/agent/mcp/reconnect" => new AgentToolsResponse([], ["http-fixture:MCP_AUTH_REQUIRED"], [diagnostic]),
        "/api/agent/skills" => new AgentSkillsResponse([skillMetadata]),
        "/api/agent/skills/import" => new AgentSkillImportResponse(false, true, skillMetadata),
        _ => throw new InvalidOperationException("Unexpected transport route.")
    };
    return new HttpResponseMessage(HttpStatusCode.OK) { Content = JsonContent.Create(response) };
})) { BaseAddress = http.BaseAddress })
using (var transports = new AgentApiClient(transportHttp))
{
    var parsed = await transports.GetConfigAsync();
    Check(parsed.McpServers[0].Cwd == stdio.Cwd && parsed.McpServers[0].EnvRefs!["THIRD_PARTY_TOKEN"] == "FIXTURE_TOKEN",
        "Stdio cwd and environment references must survive configuration reads without resolving secrets.");
    Check(parsed.McpServers[1].Transport == "streamable-http" && parsed.McpServers[1].Auth?.ClientSecretEnv == "FIXTURE_CLIENT_SECRET",
        "HTTP and OAuth client credentials must retain environment reference names only.");
    await transports.SaveConfigAsync(new(1, 10, richer.McpServers, [], richer.DisabledSkills));
    var sent = transportCalls[^1].Body!.Value;
    Check(sent.GetProperty("disabledSkills")[0].GetString() == "disabled-skill" &&
        sent.GetProperty("mcpServers")[1].GetProperty("headerEnv").GetProperty("Authorization").GetString() == "FIXTURE_AUTHORIZATION",
        "Saving server configuration preserves disabled skills and header references.");
    Check(!sent.GetProperty("mcpServers")[0].TryGetProperty("headerEnv", out _) &&
        !sent.GetProperty("mcpServers")[1].TryGetProperty("env", out _) &&
        !sent.GetProperty("mcpServers")[1].GetProperty("auth").TryGetProperty("scope", out _),
        "C# transport payloads omit optional null maps and authentication fields for gateway compatibility.");
    Check(sent.GetProperty("mcpServers")[0].GetProperty("startupTimeoutMs").GetInt32() == 60000,
        "Saving a preset preserves its configured cold-start allowance.");
    var catalog = await transports.GetMcpCatalogAsync();
    Check(catalog.Presets.Single().Server.Enabled == false && catalog.ReusedCapabilities.Contains("memory") &&
        transportCalls[^1].Method == "GET", "Preset discovery must reuse existing capabilities without starting a server.");
    var preset = catalog.Presets.Single();
    Check(preset.Package?.Version == "1.2.3" && preset.License == "MIT" && preset.Publisher == "vendor" &&
        preset.Requirements!.Single().Name == "FIXTURE_REFERENCE" && preset.ConfigurationTemplate == "dsn = \"${DSN}\"",
        "Public preset metadata retains exact package versions and configuration references without resolving credentials.");
    await transports.AddMcpPresetAsync("browser", 10);
    Check(transportCalls[^1].Path == "/api/agent/mcp/catalog/browser/add" &&
        transportCalls[^1].Body!.Value.GetProperty("expectedRevision").GetInt64() == 10, "Adding a pinned preset binds the configuration revision.");
    Check((await transports.DisconnectMcpAsync(remote.Id)).Connections.Single().State == "disconnected" &&
        transportCalls[^1].Body!.Value.GetProperty("serverId").GetString() == remote.Id, "Disconnect operates on the exact selected server.");
    var connected = await transports.ReconnectMcpAsync(remote.Id, conversationId);
    Check(connected.Connections!.Single().State == "auth-required" && connected.Errors!.Length == 1 &&
        transportCalls[^1].Path.EndsWith("?conversationId=" + conversationId.ToString("D")), "Reconnect retains context and exposes authentication state without credentials.");
    var skills = await transports.GetSkillsAsync();
    Check(!skills.Single().Enabled && skills.Single().StandardCompliant == false && skills.Single().Diagnostics!.Single().Code == "SKILL_NAME_CONFLICT" &&
        skills.Single().Conflict!.PreferredId == "preferred", "Skill status and source conflicts must remain visible even when disabled.");
    var imported = await transports.ImportSkillAsync("C:\\FixturePackage");
    Check(imported.Reused && !imported.Imported && transportCalls[^1].Body!.Value.GetProperty("directory").GetString() == "C:\\FixturePackage",
        "Skill import sends the chosen package directory and preserves reuse status.");
}
Console.WriteLine($"Agent client smoke passed: {checks} checks.");

internal sealed class Handler(Func<HttpRequestMessage, CancellationToken, Task<HttpResponseMessage>> send) : HttpMessageHandler
{
    protected override Task<HttpResponseMessage> SendAsync(HttpRequestMessage request, CancellationToken cancellationToken) => send(request, cancellationToken);
}

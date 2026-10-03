using System.Net;
using System.Text.Json;
using KYNXA.Contracts;
using KYNXA_Desktop.Services;

namespace AgentUiSmoke;

internal sealed class FakeAgentApi(string directory) : IAgentApi, IDisposable
{
    public AgentConfig Config { get; private set; } = new(1, 4,
        [new("example", "Example MCP / 原样", "example-mcp", ["--stdio"], false)], [directory]);
    public int Reads { get; private set; }
    public int Saves { get; private set; }
    public int Connections { get; private set; }
    public int CatalogReads { get; private set; }
    public int PresetAdds { get; private set; }
    public int Reconnects { get; private set; }
    public int Disconnects { get; private set; }
    public string ConnectionState { get; set; } = "disconnected";
    public string? NextReconnectState { get; set; }
    public bool IncludeDiagnostics { get; set; }
    private static McpServerConfig BrowserPreset => new("browser-fixture", "Browser fixture", "fixture-browser", ["--version=1.2.3"], false, StartupTimeoutMs: 60000);
    public int PreviewReads { get; private set; }
    public bool ConflictNextSave { get; set; }
    public bool Disposed { get; private set; }
    public TaskCompletionSource<AgentSkillDetail>? DelayedPreview { get; set; }
    public CancellationToken LastPreviewToken { get; private set; }
    public string[]? ConnectionErrors { get; set; }
    public bool IncludeMcpTools { get; private set; }
    public int ResultPages { get; private set; }
    public int ResultReads { get; private set; }
    public int LastResultOffset { get; private set; }
    public CancellationToken LastResultToken { get; private set; }
    public TaskCompletionSource<ToolResultPage>? DelayedResult { get; set; }
    public ToolResultReference ResultReference { get; } = new(Guid.Parse("315260a9-0f23-490c-8f12-ceb61a6c11a3"), 24000, new string('a', 64));
    public string ResultText => new string('A', 16000) + "\n尾文 <script>neverExecuted()</script>";
    public void InstallMcpTools()
    {
        Config = Config with { Revision = Config.Revision + 1, McpServers = [new("example", "Example MCP", "example-mcp", [], true, DisabledTools: [])] };
        IncludeMcpTools = true;
    }
    public string PreviewText => "# Example skill\nLiteral fixture instructions: run scripts.\n<script>neverExecuted()</script>\n技能原文不随语言切换。";

    public Task<AgentConfig> GetConfigAsync(CancellationToken cancellationToken = default) { Reads++; return Task.FromResult(Config); }
    public Task<AgentConfig> SaveConfigAsync(AgentConfigSaveRequest request, CancellationToken cancellationToken = default)
    {
        Saves++;
        if (ConflictNextSave) { ConflictNextSave = false; Config = Config with { Revision = Config.Revision + 1 }; throw new GatewayApiException("Example conflict", HttpStatusCode.Conflict, "AGENT_CONFIG_CONFLICT"); }
        if (request.ExpectedRevision != Config.Revision) throw new InvalidOperationException("Fixture scope revision mismatch.");
        Config = new(1, Config.Revision + 1, request.McpServers, request.SkillDirectories, request.DisabledSkills);
        return Task.FromResult(Config);
    }
    public Task<AgentSkill[]> GetSkillsAsync(Guid? conversationId = null, CancellationToken cancellationToken = default) =>
        Task.FromResult<AgentSkill[]>([new("skill-one", "Example skill", "Literal preview fixture", Path.Combine(directory, "SKILL.md"),
            Enabled: !(Config.DisabledSkills ?? []).Contains("skill-one"), StandardCompliant: !IncludeDiagnostics,
            Diagnostics: IncludeDiagnostics ? [new("UNKNOWN_SKILL_FIELD", "Fixture author metadata", "warning", "author")] : null),
            new("skill-two", "Second skill", "Second fixture", Path.Combine(directory, "other", "SKILL.md")),
            new("skill-unavailable", "Unavailable fixture", "Fixture parse error", Path.Combine(directory, "broken", "SKILL.md"),
                Diagnostics: [new("INVALID_APP_SKILL", "Fixture parse error")], Status: "unavailable")]);
    public Task<AgentSkillDetail> GetSkillAsync(string id, Guid? conversationId = null, CancellationToken cancellationToken = default)
    { PreviewReads++; LastPreviewToken = cancellationToken; return DelayedPreview?.Task ?? Task.FromResult(new AgentSkillDetail(id, "Example skill", "Preview", Path.Combine(directory, "SKILL.md"), PreviewText)); }
    public Task<AgentToolsResponse> GetToolsAsync(CancellationToken cancellationToken = default) => Task.FromResult(new AgentToolsResponse(Tools(), Connections: Diagnostics()));
    public Task<AgentToolsResponse> RefreshMcpAsync(Guid? conversationId = null, CancellationToken cancellationToken = default)
    { Connections++; return Task.FromResult(new AgentToolsResponse(Tools(), ConnectionErrors, Diagnostics())); }
    public Task<McpCatalogResponse> GetMcpCatalogAsync(CancellationToken cancellationToken = default)
    {
        CatalogReads++;
        bool exists = Config.McpServers.Any(server => server.Id == BrowserPreset.Id);
        return Task.FromResult(new McpCatalogResponse([new("browser", "Browser fixture", "Pinned browser preset", BrowserPreset, exists,
            "https://source.test.invalid/browser", ["browser"], exists ? BrowserPreset.Id : null)], ["filesystem", "memory"]));
    }
    public Task<AgentConfig> AddMcpPresetAsync(string id, long expectedRevision, CancellationToken cancellationToken = default)
    {
        if (id != "browser" || expectedRevision != Config.Revision) throw new InvalidOperationException("Fixture preset revision mismatch.");
        PresetAdds++;
        if (!Config.McpServers.Any(server => server.Id == BrowserPreset.Id)) Config = Config with { Revision = Config.Revision + 1,
            McpServers = [.. Config.McpServers, BrowserPreset] };
        return Task.FromResult(Config);
    }
    public Task<AgentToolsResponse> ReconnectMcpAsync(string serverId, Guid? conversationId = null, CancellationToken cancellationToken = default)
    { Reconnects++; ConnectionState = NextReconnectState ?? "ready"; NextReconnectState = null;
        return Task.FromResult(new AgentToolsResponse(Tools(), Connections: Diagnostics())); }
    public Task<McpConnectionsResponse> DisconnectMcpAsync(string serverId, CancellationToken cancellationToken = default)
    { Disconnects++; ConnectionState = "disconnected"; return Task.FromResult(new McpConnectionsResponse(Diagnostics())); }
    private McpConnectionDiagnostic[] Diagnostics() => Config.McpServers.Select(server => new McpConnectionDiagnostic(server.Id,
        server.Transport, ConnectionState, ConnectionState == "auth-required" ? "MCP_AUTH_REQUIRED" : null,
        ConnectionState == "ready" ? 1 : 0, new(true, true))).ToArray();
    private AgentTool[] Tools()
    {
        var tools = new List<AgentTool> { new("file.read", "Read a scoped file", JsonSerializer.SerializeToElement(new { type = "object" }), "builtin") };
        if (IncludeMcpTools) tools.Add(new("mcp.example.raw.tool", "External fixture tool", JsonSerializer.SerializeToElement(new { type = "object" }),
            "mcp:example", "raw.tool", !(Config.McpServers.FirstOrDefault(server => server.Id == "example")?.DisabledTools ?? []).Contains("raw.tool")));
        return tools.ToArray();
    }
    public Task<ToolResultPage> GetToolResultPageAsync(Guid conversationId, ToolResultReference reference, int offset = 0,
        int limit = 16000, CancellationToken cancellationToken = default)
    {
        ResultPages++; LastResultOffset = offset; LastResultToken = cancellationToken;
        if (reference != ResultReference || conversationId == Guid.Empty) throw new InvalidOperationException("Fixture reference identity mismatch.");
        string text = ResultText.Substring(offset, Math.Min(limit, ResultText.Length - offset));
        return DelayedResult?.Task ?? Task.FromResult(new ToolResultPage(reference.Id, text, ResultText.Length, offset, offset + text.Length,
            offset + text.Length < ResultText.Length, reference));
    }
    public Task<ToolResultResponse> GetToolResultAsync(Guid conversationId, ToolResultReference reference, CancellationToken cancellationToken = default)
    {
        ResultReads++;
        return Task.FromResult(new ToolResultResponse(JsonSerializer.SerializeToElement(new { content = new object[] {
            new { type = "image", mimeType = "image/gif", data = "R0lGODlhAQABAIAAAAAAAP///yH5BAEAAAAALAAAAAABAAEAAAIBRAA7" },
            new { type = "resource_link", uri = "resource://fixture/report", mimeType = "text/plain", name = "Fixture resource" },
            new { type = "audio", mimeType = "audio/wav", data = "fixture-data" }
        } })));
    }
    public Task<AgentApprovalResponse> SubmitApprovalAsync(AgentApprovalRequest request, CancellationToken cancellationToken = default) =>
        throw new InvalidOperationException("The management UI must not approve tools.");
    public void Dispose() => Disposed = true;
}

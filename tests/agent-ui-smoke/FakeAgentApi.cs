using System.Net;
using System.Text.Json;
using KYNXA.Contracts;
using KYNXA_Desktop.Services;

namespace AgentUiSmoke;

internal sealed class FakeAgentApi(string directory) : IAgentApi, IDisposable
{
    public AgentConfig Config { get; private set; } = new(1, 4,
        [new("example", "Example MCP / 原样", "example-mcp", ["--stdio"], false)], [directory]);
    public AgentConfigSaveRequest? LastSaveRequest { get; private set; }
    public AgentConfig? NextSaveResponse { get; set; }
    private bool _officialLayers;
    public McpServerConfig OfficialBrowserDefault => BrowserPreset with { Id = "official-browser", Origin = "official", PresetId = "browser", Overridden = false };
    public void InstallOfficialLayers()
    {
        _officialLayers = true;
        Config = Config with { McpServers = [OfficialBrowserDefault,
            new("user-fixture", "User fixture / 用户配置", "fixture-user", [], false, Origin: "user")],
            DisabledOfficialMcpServers = ["hidden-fixture"], OfficialToolsRoot = Path.Combine(directory, "Official", "1.0.0"),
            UserToolsRoot = Path.Combine(directory, "User"), OfficialPackageVersion = "1.0.0" };
    }
    public void InstallBrowserFixtures()
    {
        Config = Config with { McpServers = [
            new("playwright-fixture", "Playwright / 浏览器", "npx", ["-y", "@playwright/mcp@0.0.83",
                "--user-data-dir", Path.Combine(directory, "FixtureProfile"), "--profile-dir-name", "Profile 1", "--viewport-size", "1440,900"], false),
            new("chrome-fixture", "Chrome DevTools / 浏览器", "npx", ["-y", "chrome-devtools-mcp@1.10.1", "--headless", "--isolated", "--no-usage-statistics"], false),
            new("environment-fixture", "Environment configured / 环境配置", "npx", ["-y", "@playwright/mcp@0.0.83"], false,
                EnvRefs: new() { ["PLAYWRIGHT_MCP_CDP_ENDPOINT"] = "FAKE_BROWSER_ENDPOINT_ENV" }),
            new("config-fixture", "Advanced config / 配置文件", "npx", ["-y", "@playwright/mcp@0.0.83", "--config", "fixture-browser.json"], false)
        ] };
    }
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
    public string? ScreenshotImagePath { get; set; }
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
        LastSaveRequest = request;
        if (ConflictNextSave) { ConflictNextSave = false; Config = Config with { Revision = Config.Revision + 1 }; throw new GatewayApiException("Example conflict", HttpStatusCode.Conflict, "AGENT_CONFIG_CONFLICT"); }
        if (request.ExpectedRevision != Config.Revision) throw new InvalidOperationException("Fixture scope revision mismatch.");
        Config = NextSaveResponse ?? Config with { Revision = Config.Revision + 1, McpServers = request.McpServers,
            SkillDirectories = request.SkillDirectories, DisabledSkills = request.DisabledSkills,
            DisabledOfficialMcpServers = request.DisabledOfficialMcpServers };
        NextSaveResponse = null;
        return Task.FromResult(Config);
    }
    public Task<AgentSkill[]> GetSkillsAsync(Guid? conversationId = null, CancellationToken cancellationToken = default) =>
        Task.FromResult<AgentSkill[]>([new("skill-one", "Example skill", "Literal preview fixture", Path.Combine(directory, "SKILL.md"),
            Enabled: !(Config.DisabledSkills ?? []).Contains("skill-one"), StandardCompliant: !IncludeDiagnostics,
            Diagnostics: IncludeDiagnostics ? [new("UNKNOWN_SKILL_FIELD", "Fixture author metadata", "warning", "author")] : null,
            Origin: _officialLayers ? "builtin" : null),
            new("skill-two", "Second skill", "Second fixture", Path.Combine(directory, "other", "SKILL.md"), Origin: _officialLayers ? "configured" : null),
            new("skill-unavailable", "Unavailable fixture", "Fixture parse error", Path.Combine(directory, "broken", "SKILL.md"),
                Diagnostics: [new("INVALID_APP_SKILL", "Fixture parse error")], Status: "unavailable", Origin: _officialLayers ? "workspace" : null)]);
    public Task<AgentSkillDetail> GetSkillAsync(string id, Guid? conversationId = null, CancellationToken cancellationToken = default)
    { PreviewReads++; LastPreviewToken = cancellationToken; return DelayedPreview?.Task ?? Task.FromResult(new AgentSkillDetail(id, "Example skill", "Preview", Path.Combine(directory, "SKILL.md"), PreviewText)); }
    public Task<AgentToolsResponse> GetToolsAsync(CancellationToken cancellationToken = default) => Task.FromResult(new AgentToolsResponse(Tools(), Connections: Diagnostics()));
    public Task<AgentToolsResponse> RefreshMcpAsync(Guid? conversationId = null, CancellationToken cancellationToken = default)
    { Connections++; return Task.FromResult(new AgentToolsResponse(Tools(), ConnectionErrors, Diagnostics())); }
    public Task<McpCatalogResponse> GetMcpCatalogAsync(CancellationToken cancellationToken = default)
    {
        CatalogReads++;
        var configured = Config.McpServers.FirstOrDefault(server => server.Id == BrowserPreset.Id || server.PresetId == "browser");
        bool exists = configured is not null;
        return Task.FromResult(new McpCatalogResponse([new("browser", "Browser fixture", "Pinned browser preset", BrowserPreset, exists,
            "https://source.test.invalid/browser", ["browser"], configured?.Id)], ["filesystem", "memory"]));
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
        if (reference != ResultReference || conversationId == Guid.Empty) throw new InvalidOperationException("Fixture media reference identity mismatch.");
        if (ScreenshotImagePath is { } imagePath)
            return Task.FromResult(new ToolResultResponse(JsonSerializer.SerializeToElement(new { content = new object[] {
                new { type = "image", mimeType = "image/png", data = Convert.ToBase64String(File.ReadAllBytes(imagePath)) }
            }, structuredContent = new { completed = true, boundary = "host-desktop", action = "screenshot" } })));
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

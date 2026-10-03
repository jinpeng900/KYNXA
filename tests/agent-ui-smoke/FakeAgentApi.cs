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
    public int PreviewReads { get; private set; }
    public bool ConflictNextSave { get; set; }
    public bool Disposed { get; private set; }
    public TaskCompletionSource<AgentSkillDetail>? DelayedPreview { get; set; }
    public CancellationToken LastPreviewToken { get; private set; }
    public string[]? ConnectionErrors { get; set; }
    public string PreviewText => "# Example skill\nLiteral fixture instructions: run scripts.\n<script>neverExecuted()</script>\n技能原文不随语言切换。";

    public Task<AgentConfig> GetConfigAsync(CancellationToken cancellationToken = default) { Reads++; return Task.FromResult(Config); }
    public Task<AgentConfig> SaveConfigAsync(AgentConfigSaveRequest request, CancellationToken cancellationToken = default)
    {
        Saves++;
        if (ConflictNextSave) { ConflictNextSave = false; Config = Config with { Revision = Config.Revision + 1 }; throw new GatewayApiException("Example conflict", HttpStatusCode.Conflict, "AGENT_CONFIG_CONFLICT"); }
        if (request.ExpectedRevision != Config.Revision) throw new InvalidOperationException("Fixture scope revision mismatch.");
        Config = new(1, Config.Revision + 1, request.McpServers, request.SkillDirectories);
        return Task.FromResult(Config);
    }
    public Task<AgentSkill[]> GetSkillsAsync(Guid? conversationId = null, CancellationToken cancellationToken = default) =>
        Task.FromResult<AgentSkill[]>([new("skill-one", "Example skill", "Literal preview fixture", Path.Combine(directory, "SKILL.md")),
            new("skill-two", "Second skill", "Second fixture", Path.Combine(directory, "other", "SKILL.md"))]);
    public Task<AgentSkillDetail> GetSkillAsync(string id, Guid? conversationId = null, CancellationToken cancellationToken = default)
    { PreviewReads++; LastPreviewToken = cancellationToken; return DelayedPreview?.Task ?? Task.FromResult(new AgentSkillDetail(id, "Example skill", "Preview", Path.Combine(directory, "SKILL.md"), PreviewText)); }
    public Task<AgentToolsResponse> GetToolsAsync(CancellationToken cancellationToken = default) => Task.FromResult(new AgentToolsResponse(Tools()));
    public Task<AgentToolsResponse> RefreshMcpAsync(Guid? conversationId = null, CancellationToken cancellationToken = default)
    { Connections++; return Task.FromResult(new AgentToolsResponse(Tools(), ConnectionErrors)); }
    private static AgentTool[] Tools() => [new("file.read", "Read a scoped file", JsonSerializer.SerializeToElement(new { type = "object" }), "builtin")];
    public Task<AgentApprovalResponse> SubmitApprovalAsync(AgentApprovalRequest request, CancellationToken cancellationToken = default) =>
        throw new InvalidOperationException("The management UI must not approve tools.");
    public void Dispose() => Disposed = true;
}

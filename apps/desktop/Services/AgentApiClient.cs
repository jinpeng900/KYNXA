using System.Net.Http.Json;
using System.Text.Json;
using KYNXA.Contracts;

namespace KYNXA_Desktop.Services;

public interface IAgentApi
{
    Task<AgentConfig> GetConfigAsync(CancellationToken cancellationToken = default);
    Task<AgentConfig> SaveConfigAsync(AgentConfigSaveRequest request, CancellationToken cancellationToken = default);
    Task<AgentSkill[]> GetSkillsAsync(Guid? conversationId = null, CancellationToken cancellationToken = default);
    Task<AgentSkillDetail> GetSkillAsync(string id, Guid? conversationId = null, CancellationToken cancellationToken = default);
    Task<AgentToolsResponse> GetToolsAsync(CancellationToken cancellationToken = default);
    Task<AgentToolsResponse> RefreshMcpAsync(Guid? conversationId = null, CancellationToken cancellationToken = default);
    Task<AgentApprovalResponse> SubmitApprovalAsync(AgentApprovalRequest request, CancellationToken cancellationToken = default);
}

public sealed class AgentApiClient : IAgentApi, IDisposable
{
    private static readonly JsonSerializerOptions JsonOptions = new(JsonSerializerDefaults.Web);
    private readonly HttpClient _http;
    private readonly Func<CancellationToken, Task> _ensureReady;
    private readonly bool _ownsHttpClient;

    public AgentApiClient() : this(new HttpClient { BaseAddress = ModelGatewayService.Address,
        Timeout = TimeSpan.FromSeconds(30) }, ModelGatewayService.EnsureReadyAsync, true) { }

    public AgentApiClient(HttpClient http, Func<CancellationToken, Task>? ensureReady = null, bool ownsHttpClient = false)
    {
        _http = http;
        _ensureReady = ensureReady ?? (_ => Task.CompletedTask);
        _ownsHttpClient = ownsHttpClient;
    }

    public Task<AgentConfig> GetConfigAsync(CancellationToken cancellationToken = default) =>
        SendAsync<AgentConfig>(HttpMethod.Get, "/api/agent/config", null, cancellationToken);

    public Task<AgentConfig> SaveConfigAsync(AgentConfigSaveRequest request, CancellationToken cancellationToken = default)
    {
        if (request.Version != 1 || request.ExpectedRevision < 0) throw new ArgumentException("Invalid agent configuration version.");
        return SendAsync<AgentConfig>(HttpMethod.Put, "/api/agent/config", request, cancellationToken);
    }

    public async Task<AgentSkill[]> GetSkillsAsync(Guid? conversationId = null, CancellationToken cancellationToken = default) =>
        (await SendAsync<AgentSkillsResponse>(HttpMethod.Get, WithConversation("/api/agent/skills", conversationId), null, cancellationToken)).Skills;

    public async Task<AgentSkillDetail> GetSkillAsync(string id, Guid? conversationId = null, CancellationToken cancellationToken = default)
    {
        if (string.IsNullOrWhiteSpace(id)) throw new ArgumentException("A skill ID is required.", nameof(id));
        var skill = (await SendAsync<AgentSkillResponse>(HttpMethod.Get, WithConversation("/api/agent/skills/" + Uri.EscapeDataString(id), conversationId),
            null, cancellationToken)).Skill;
        if (skill is null || skill.Id != id) throw new InvalidDataException(UiText.Get("工具接口返回了空响应。"));
        return skill;
    }

    public Task<AgentToolsResponse> GetToolsAsync(CancellationToken cancellationToken = default) =>
        SendAsync<AgentToolsResponse>(HttpMethod.Get, "/api/agent/tools", null, cancellationToken);

    public Task<AgentToolsResponse> RefreshMcpAsync(Guid? conversationId = null, CancellationToken cancellationToken = default) =>
        SendAsync<AgentToolsResponse>(HttpMethod.Post, WithConversation("/api/agent/mcp/refresh", conversationId),
            null, cancellationToken);

    public async Task<AgentApprovalResponse> SubmitApprovalAsync(AgentApprovalRequest request, CancellationToken cancellationToken = default)
    {
        if (request.ConversationId == Guid.Empty || request.RequestId == Guid.Empty || request.ApprovalId == Guid.Empty ||
            string.IsNullOrWhiteSpace(request.ToolCallId)) throw new ArgumentException("An approval must identify its exact request and tool call.");
        var result = await SendAsync<AgentApprovalResponse>(HttpMethod.Post, "/api/agent/approvals", request, cancellationToken);
        if (result.ApprovalId != request.ApprovalId || result.ToolCallId != request.ToolCallId || result.Approved != request.Approved)
            throw new InvalidDataException(UiText.Get("工具接口返回了无效的审批响应。"));
        return result;
    }

    private static string WithConversation(string path, Guid? conversationId) => conversationId is { } id
        ? path + "?conversationId=" + Uri.EscapeDataString(id.ToString("D")) : path;

    private async Task<T> SendAsync<T>(HttpMethod method, string path, object? payload, CancellationToken cancellationToken)
    {
        JsonElement? snapshot = payload is null ? null : JsonSerializer.SerializeToElement(payload, JsonOptions);
        await _ensureReady(cancellationToken);
        using var request = new HttpRequestMessage(method, path);
        if (snapshot is { } body) request.Content = JsonContent.Create(body, options: JsonOptions);
        using var response = await _http.SendAsync(request, cancellationToken);
        return await GatewayResponseReader.ReadAsync<T>(response, UiText.Get("工具接口返回了空响应。"),
            UiText.Get("工具接口返回 HTTP {0}。"), cancellationToken, JsonOptions);
    }

    public void Dispose()
    {
        if (_ownsHttpClient) _http.Dispose();
    }
}

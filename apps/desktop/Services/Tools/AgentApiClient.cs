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
    Task<AgentSkillImportResponse> ImportSkillAsync(string directory, CancellationToken cancellationToken = default) => throw new NotSupportedException();
    Task<AgentToolsResponse> GetToolsAsync(CancellationToken cancellationToken = default);
    Task<AgentToolsResponse> RefreshMcpAsync(Guid? conversationId = null, CancellationToken cancellationToken = default);
    Task<McpCatalogResponse> GetMcpCatalogAsync(CancellationToken cancellationToken = default) =>
        Task.FromResult(new McpCatalogResponse([], []));
    Task<AgentConfig> AddMcpPresetAsync(string id, long expectedRevision, CancellationToken cancellationToken = default) => throw new NotSupportedException();
    Task<McpConnectionsResponse> DisconnectMcpAsync(string serverId, CancellationToken cancellationToken = default) => throw new NotSupportedException();
    Task<AgentToolsResponse> ReconnectMcpAsync(string serverId, Guid? conversationId = null, CancellationToken cancellationToken = default) => throw new NotSupportedException();
    Task<AgentApprovalResponse> SubmitApprovalAsync(AgentApprovalRequest request, CancellationToken cancellationToken = default);
    Task<ToolResultPage> GetToolResultPageAsync(Guid conversationId, ToolResultReference reference, int offset = 0,
        int limit = 16000, CancellationToken cancellationToken = default) => throw new NotSupportedException();
    Task<ToolResultResponse> GetToolResultAsync(Guid conversationId, ToolResultReference reference,
        CancellationToken cancellationToken = default) => throw new NotSupportedException();
}

public sealed class AgentApiClient : IAgentApi, IDisposable
{
    private static readonly JsonSerializerOptions JsonOptions = new(JsonSerializerDefaults.Web)
    { DefaultIgnoreCondition = System.Text.Json.Serialization.JsonIgnoreCondition.WhenWritingNull };
    private readonly HttpClient _httpClient;
    private readonly Func<CancellationToken, Task> _ensureGatewayReady;
    private readonly bool _ownsHttpClient;

    public AgentApiClient() : this(new HttpClient { BaseAddress = ModelGatewayService.Address,
        Timeout = TimeSpan.FromSeconds(150) }, ModelGatewayService.EnsureReadyAsync, true) { }

    public AgentApiClient(HttpClient http, Func<CancellationToken, Task>? ensureReady = null, bool ownsHttpClient = false)
    {
        _httpClient = http;
        _ensureGatewayReady = ensureReady ?? (_ => Task.CompletedTask);
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

    public Task<AgentSkillImportResponse> ImportSkillAsync(string directory, CancellationToken cancellationToken = default)
    {
        if (string.IsNullOrWhiteSpace(directory) || !Path.IsPathFullyQualified(directory) || directory.Contains('\0'))
            throw new ArgumentException("A fully qualified skill package directory is required.");
        return SendAsync<AgentSkillImportResponse>(HttpMethod.Post, "/api/agent/skills/import", new { directory }, cancellationToken);
    }

    public Task<AgentToolsResponse> RefreshMcpAsync(Guid? conversationId = null, CancellationToken cancellationToken = default) =>
        SendAsync<AgentToolsResponse>(HttpMethod.Post, WithConversation("/api/agent/mcp/refresh", conversationId),
            null, cancellationToken);

    public Task<McpCatalogResponse> GetMcpCatalogAsync(CancellationToken cancellationToken = default) =>
        SendAsync<McpCatalogResponse>(HttpMethod.Get, "/api/agent/mcp/catalog", null, cancellationToken);

    public Task<AgentConfig> AddMcpPresetAsync(string id, long expectedRevision, CancellationToken cancellationToken = default)
    {
        if (string.IsNullOrWhiteSpace(id) || expectedRevision < 0) throw new ArgumentException("A preset and configuration revision are required.");
        return SendAsync<AgentConfig>(HttpMethod.Post, "/api/agent/mcp/catalog/" + Uri.EscapeDataString(id) + "/add",
            new { expectedRevision }, cancellationToken);
    }

    public Task<McpConnectionsResponse> DisconnectMcpAsync(string serverId, CancellationToken cancellationToken = default) =>
        SendAsync<McpConnectionsResponse>(HttpMethod.Post, "/api/agent/mcp/disconnect", ServerIdentity(serverId), cancellationToken);

    public Task<AgentToolsResponse> ReconnectMcpAsync(string serverId, Guid? conversationId = null, CancellationToken cancellationToken = default) =>
        SendAsync<AgentToolsResponse>(HttpMethod.Post, WithConversation("/api/agent/mcp/reconnect", conversationId), ServerIdentity(serverId), cancellationToken);

    private static object ServerIdentity(string serverId)
    {
        if (string.IsNullOrWhiteSpace(serverId)) throw new ArgumentException("A server identity is required.");
        return new { serverId };
    }

    public async Task<AgentApprovalResponse> SubmitApprovalAsync(AgentApprovalRequest request, CancellationToken cancellationToken = default)
    {
        if (request.ConversationId == Guid.Empty || request.RequestId == Guid.Empty || request.ApprovalId == Guid.Empty ||
            string.IsNullOrWhiteSpace(request.ToolCallId)) throw new ArgumentException("An approval must identify its exact request and tool call.");
        var result = await SendAsync<AgentApprovalResponse>(HttpMethod.Post, "/api/agent/approvals", request, cancellationToken);
        if (result.ApprovalId != request.ApprovalId || result.ToolCallId != request.ToolCallId || result.Approved != request.Approved)
            throw new InvalidDataException(UiText.Get("工具接口返回了无效的审批响应。"));
        return result;
    }

    public async Task<ToolResultPage> GetToolResultPageAsync(Guid conversationId, ToolResultReference reference, int offset = 0,
        int limit = 16000, CancellationToken cancellationToken = default)
    {
        if (offset < 0 || limit is < 1 or > 16000) throw new ArgumentOutOfRangeException(nameof(offset));
        var page = await SendAsync<ToolResultPage>(HttpMethod.Get, ResultPath(conversationId, reference) +
            $"?offset={offset}&limit={limit}", null, cancellationToken);
        bool adjustedOffset = page.Offset == offset - 1 && page.Text is { Length: >= 2 } &&
            char.IsSurrogatePair(page.Text[0], page.Text[1]);
        bool completePair = limit == 1 && page.Text is { Length: 2 } && char.IsSurrogatePair(page.Text[0], page.Text[1]);
        if (page.Id != reference.Id || page.ResultRef != reference || (page.Offset != offset && !adjustedOffset) || page.Text is null ||
            (page.Text.Length > limit && !completePair) || page.TotalCharacters < offset || page.NextOffset != page.Offset + page.Text.Length ||
            page.NextOffset > page.TotalCharacters || page.Truncated != (page.NextOffset < page.TotalCharacters) ||
            (page.Truncated && page.Text.Length == 0)) throw new InvalidDataException(UiText.Get("工具结果响应无效。"));
        return page;
    }

    public async Task<ToolResultResponse> GetToolResultAsync(Guid conversationId, ToolResultReference reference,
        CancellationToken cancellationToken = default)
    {
        var response = await SendAsync<ToolResultResponse>(HttpMethod.Get, ResultPath(conversationId, reference), null, cancellationToken);
        if (response.Result.ValueKind != JsonValueKind.Object) throw new InvalidDataException(UiText.Get("工具结果响应无效。"));
        return response;
    }

    private static string ResultPath(Guid conversationId, ToolResultReference reference)
    {
        if (conversationId == Guid.Empty || reference.Id == Guid.Empty || reference.Bytes < 0 ||
            string.IsNullOrEmpty(reference.Sha256) || reference.Sha256.Length != 64 || reference.Sha256.Any(value => !Uri.IsHexDigit(value)))
            throw new ArgumentException("A tool result must identify its conversation and reference.");
        return $"/api/conversations/{conversationId:D}/tool-results/{reference.Id:D}";
    }

    private static string WithConversation(string path, Guid? conversationId) => conversationId is { } id
        ? path + "?conversationId=" + Uri.EscapeDataString(id.ToString("D")) : path;

    private async Task<T> SendAsync<T>(HttpMethod method, string path, object? payload, CancellationToken cancellationToken)
    {
        JsonElement? snapshot = payload is null ? null : JsonSerializer.SerializeToElement(payload, JsonOptions);
        await _ensureGatewayReady(cancellationToken);
        using var request = new HttpRequestMessage(method, path);
        if (snapshot is { } body) request.Content = JsonContent.Create(body, options: JsonOptions);
        using var response = await _httpClient.SendAsync(request, cancellationToken);
        return await GatewayResponseReader.ReadAsync<T>(response, UiText.Get("工具接口返回了空响应。"),
            UiText.Get("工具接口返回 HTTP {0}。"), cancellationToken, JsonOptions);
    }

    public void Dispose()
    {
        if (_ownsHttpClient) _httpClient.Dispose();
    }
}

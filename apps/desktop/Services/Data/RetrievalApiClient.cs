using System.Net.Http.Json;
using System.Text.Json;
using KYNXA.Contracts;

namespace KYNXA_Desktop.Services;

public interface IRetrievalApi
{
    Task<RetrievalSettingsDocument> GetSettingsAsync(CancellationToken cancellationToken = default);
    Task<RetrievalSettingsDocument> SaveSettingsAsync(RetrievalSettingsUpdateRequest request, CancellationToken cancellationToken = default);
    Task<ProjectRetrievalSettingsDocument> GetProjectSettingsAsync(Guid projectId, CancellationToken cancellationToken = default);
    Task<ProjectRetrievalSettingsDocument> SaveProjectSettingsAsync(Guid projectId, ProjectRetrievalSettingsUpdateRequest request, CancellationToken cancellationToken = default);
    Task<RetrievalStatus> GetStatusAsync(CancellationToken cancellationToken = default);
    Task<RetrievalProvidersResponse> GetProvidersAsync(CancellationToken cancellationToken = default);
    Task<RetrievalSourcesResponse> GetSourcesAsync(Guid? projectId = null, CancellationToken cancellationToken = default);
    Task<RetrievalSource> ImportSourceAsync(RetrievalSourceImportRequest request, CancellationToken cancellationToken = default);
    Task DeleteSourceAsync(string sourceId, long? expectedRevision = null, CancellationToken cancellationToken = default);
    Task<RetrievalIndexJob> RebuildIndexAsync(Guid? projectId = null, CancellationToken cancellationToken = default);
    Task<RetrievalIndexJob> GetIndexJobAsync(string jobId, CancellationToken cancellationToken = default);
    Task<RetrievalIndexJob> CancelIndexJobAsync(string jobId, CancellationToken cancellationToken = default);
}

/// <summary>
/// Sends retrieval commands to the single gateway owner without creating chats or writing local indexes.
/// 将检索命令发送给唯一网关所有者，不创建聊天，也不在桌面写入索引。
/// </summary>
public sealed class RetrievalApiClient : IRetrievalApi, IDisposable
{
    private static readonly JsonSerializerOptions JsonOptions = new(JsonSerializerDefaults.Web);
    private readonly HttpClient _httpClient;
    private readonly Func<CancellationToken, Task> _ensureGatewayReady;
    private readonly bool _ownsHttpClient;

    public RetrievalApiClient() : this(new HttpClient
    {
        BaseAddress = ModelGatewayService.Address,
        Timeout = TimeSpan.FromSeconds(60)
    }, ModelGatewayService.EnsureReadyAsync, ownsHttpClient: true) { }

    public RetrievalApiClient(HttpClient httpClient, Func<CancellationToken, Task>? ensureReady = null, bool ownsHttpClient = false)
    {
        _httpClient = httpClient;
        _ensureGatewayReady = ensureReady ?? (_ => Task.CompletedTask);
        _ownsHttpClient = ownsHttpClient;
    }

    public async Task<RetrievalSettingsDocument> GetSettingsAsync(CancellationToken cancellationToken = default)
    {
        var document = await SendAsync<RetrievalSettingsDocument>(HttpMethod.Get, "/api/retrieval/settings", null, cancellationToken);
        ValidateRevision(document.SchemaVersion, document.Revision);
        return document;
    }

    public async Task<RetrievalSettingsDocument> SaveSettingsAsync(RetrievalSettingsUpdateRequest request, CancellationToken cancellationToken = default)
    {
        ValidateRevision(1, request.ExpectedRevision);
        var document = await SendAsync<RetrievalSettingsDocument>(HttpMethod.Patch, "/api/retrieval/settings", request, cancellationToken);
        ValidateRevision(document.SchemaVersion, document.Revision);
        return document;
    }

    public async Task<ProjectRetrievalSettingsDocument> GetProjectSettingsAsync(Guid projectId, CancellationToken cancellationToken = default)
    {
        var document = await SendAsync<ProjectRetrievalSettingsDocument>(HttpMethod.Get, ProjectPath(projectId), null, cancellationToken);
        ValidateProject(projectId, document);
        return document;
    }

    public async Task<ProjectRetrievalSettingsDocument> SaveProjectSettingsAsync(Guid projectId, ProjectRetrievalSettingsUpdateRequest request,
        CancellationToken cancellationToken = default)
    {
        ValidateRevision(1, request.ExpectedRevision);
        var document = await SendAsync<ProjectRetrievalSettingsDocument>(HttpMethod.Patch, ProjectPath(projectId), request, cancellationToken);
        ValidateProject(projectId, document);
        return document;
    }

    public Task<RetrievalStatus> GetStatusAsync(CancellationToken cancellationToken = default) =>
        SendAsync<RetrievalStatus>(HttpMethod.Get, "/api/retrieval/status", null, cancellationToken);
    public Task<RetrievalProvidersResponse> GetProvidersAsync(CancellationToken cancellationToken = default) =>
        SendAsync<RetrievalProvidersResponse>(HttpMethod.Get, "/api/retrieval/providers", null, cancellationToken);
    public Task<RetrievalSourcesResponse> GetSourcesAsync(Guid? projectId = null, CancellationToken cancellationToken = default) =>
        SendAsync<RetrievalSourcesResponse>(HttpMethod.Get, "/api/retrieval/sources" +
            (projectId is { } id ? $"?projectId={ValidProjectId(id):D}" : string.Empty), null, cancellationToken);
    public Task<RetrievalSource> ImportSourceAsync(RetrievalSourceImportRequest request, CancellationToken cancellationToken = default) =>
        SendAsync<RetrievalSource>(HttpMethod.Post, "/api/retrieval/sources", request, cancellationToken);

    public async Task DeleteSourceAsync(string sourceId, long? expectedRevision = null, CancellationToken cancellationToken = default)
    {
        await _ensureGatewayReady(cancellationToken);
        using var request = new HttpRequestMessage(HttpMethod.Delete, $"/api/retrieval/sources/{PathId(sourceId)}")
        {
            Content = JsonContent.Create(new RetrievalSourceDeleteRequest(expectedRevision), options: JsonOptions)
        };
        using var response = await _httpClient.SendAsync(request, cancellationToken);
        await GatewayResponseReader.EnsureSuccessAsync(response, UiText.Get("检索接口返回 HTTP {0}。"), cancellationToken);
    }

    public Task<RetrievalIndexJob> RebuildIndexAsync(Guid? projectId = null, CancellationToken cancellationToken = default) =>
        SendAsync<RetrievalIndexJob>(HttpMethod.Post, "/api/retrieval/index/rebuild", new RetrievalIndexRebuildRequest(projectId), cancellationToken);
    public Task<RetrievalIndexJob> GetIndexJobAsync(string jobId, CancellationToken cancellationToken = default) =>
        SendAsync<RetrievalIndexJob>(HttpMethod.Get, $"/api/retrieval/index/jobs/{PathId(jobId)}", null, cancellationToken);
    public Task<RetrievalIndexJob> CancelIndexJobAsync(string jobId, CancellationToken cancellationToken = default) =>
        SendAsync<RetrievalIndexJob>(HttpMethod.Post, $"/api/retrieval/index/jobs/{PathId(jobId)}/cancel", null, cancellationToken);

    private async Task<T> SendAsync<T>(HttpMethod method, string path, object? payload, CancellationToken cancellationToken)
    {
        await _ensureGatewayReady(cancellationToken);
        using var request = new HttpRequestMessage(method, path);
        if (payload is not null) request.Content = JsonContent.Create(payload, options: JsonOptions);
        using var response = await _httpClient.SendAsync(request, cancellationToken);
        return await GatewayResponseReader.ReadAsync<T>(response, UiText.Get("检索接口返回了无效响应。"),
            UiText.Get("检索接口返回 HTTP {0}。"), cancellationToken, JsonOptions);
    }

    private static string PathId(string id)
    {
        if (string.IsNullOrWhiteSpace(id) || id.Length > 128 || id.Any(character => !char.IsAsciiLetterOrDigit(character) && character is not '-' and not '_'))
            throw new ArgumentException("A valid retrieval resource ID is required.", nameof(id));
        return Uri.EscapeDataString(id);
    }

    private static Guid ValidProjectId(Guid projectId) => projectId != Guid.Empty ? projectId :
        throw new ArgumentException("A saved project ID is required.", nameof(projectId));
    private static string ProjectPath(Guid projectId) => $"/api/projects/{ValidProjectId(projectId):D}/retrieval/settings";
    private static void ValidateRevision(int schemaVersion, long revision)
    {
        if (schemaVersion != 1 || revision is < 0 or > MemoryInputValidation.MaximumRevision)
            throw new InvalidDataException(UiText.Get("检索接口返回了无效响应。"));
    }
    private static void ValidateProject(Guid projectId, ProjectRetrievalSettingsDocument document)
    {
        ValidateRevision(document.SchemaVersion, document.Revision);
        if (document.ProjectId != projectId || document.Effective is null || document.IndexingSources is null)
            throw new InvalidDataException(UiText.Get("检索接口返回了无效响应。"));
    }

    public void Dispose()
    {
        if (_ownsHttpClient) _httpClient.Dispose();
    }
}

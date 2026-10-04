using System.Net.Http.Json;
using System.Text.Json;
using KYNXA.Contracts;
using KYNXA_Desktop.ViewModels;

namespace KYNXA_Desktop.Services;

public interface IMemoryApi
{
    Task<MemoryScopeDocument> GetAsync(MemoryTarget target, CancellationToken cancellationToken = default);
    Task<MemoryScopeDocument> CreateAsync(MemoryTarget target, MemoryCreateRequest request, CancellationToken cancellationToken = default);
    Task<MemoryScopeDocument> UpdateAsync(MemoryTarget target, Guid entryId, MemoryUpdateRequest request, CancellationToken cancellationToken = default);
    Task<MemoryScopeDocument> DeleteAsync(MemoryTarget target, Guid entryId, MemoryDeleteRequest request, CancellationToken cancellationToken = default);
}

/// <summary>
/// Gateway memory transport; no drafts, snapshots, or authoritative data are written by the desktop.
/// 记忆 API 传输层；桌面不写入草稿快照或正式记忆数据。
/// </summary>
public sealed class MemoryApiClient : IMemoryApi, IDisposable
{
    private static readonly JsonSerializerOptions JsonOptions = new(JsonSerializerDefaults.Web);
    private readonly HttpClient _httpClient;
    private readonly Func<CancellationToken, Task> _ensureGatewayReady;
    private readonly bool _ownsHttpClient;

    public MemoryApiClient() : this(new HttpClient
    {
        BaseAddress = ModelGatewayService.Address,
        Timeout = TimeSpan.FromSeconds(30)
    }, ModelGatewayService.EnsureReadyAsync, ownsHttpClient: true) { }

    public MemoryApiClient(HttpClient http, Func<CancellationToken, Task>? ensureReady = null, bool ownsHttpClient = false)
    {
        _httpClient = http;
        _ensureGatewayReady = ensureReady ?? (_ => Task.CompletedTask);
        _ownsHttpClient = ownsHttpClient;
    }

    public async Task<MemoryScopeDocument> GetAsync(MemoryTarget target, CancellationToken cancellationToken = default)
    {
        string path = PathFor(target);
        await _ensureGatewayReady(cancellationToken);
        using var response = await _httpClient.GetAsync(path, cancellationToken);
        MemoryScopeDocument document;
        if (target.Scope == MemoryScopes.Chat)
        {
            var snapshot = await ReadAsync<ConversationMemoryResponse>(response, cancellationToken);
            if (snapshot.ConversationId != target.Id || snapshot.Scopes is null)
                throw new InvalidDataException(UiText.Get("记忆接口返回了空响应。"));
            document = snapshot.Scopes.SingleOrDefault(scope => scope.Scope == target.Scope && scope.ScopeId == target.ScopeId)
                ?? throw new InvalidDataException(UiText.Get("记忆接口返回了空响应。"));
        }
        else document = await ReadAsync<MemoryScopeDocument>(response, cancellationToken);
        ValidateDocument(target, document, requireSourceStatus: true);
        return document;
    }

    public Task<MemoryScopeDocument> CreateAsync(MemoryTarget target, MemoryCreateRequest request, CancellationToken cancellationToken = default)
    {
        ValidateInput(target, request.Scope, request.ExpectedRevision, request.Content, request.Kind);
        return WriteAsync(target, HttpMethod.Post, PathFor(target), request, cancellationToken);
    }

    public Task<MemoryScopeDocument> UpdateAsync(MemoryTarget target, Guid entryId, MemoryUpdateRequest request, CancellationToken cancellationToken = default)
    {
        ValidateInput(target, request.Scope, request.ExpectedRevision, request.Content, request.Kind);
        return WriteAsync(target, HttpMethod.Patch, EntryPathFor(target, entryId), request, cancellationToken);
    }

    public Task<MemoryScopeDocument> DeleteAsync(MemoryTarget target, Guid entryId, MemoryDeleteRequest request, CancellationToken cancellationToken = default)
    {
        ValidateScopeRevision(target, request.Scope, request.ExpectedRevision);
        return WriteAsync(target, HttpMethod.Delete, EntryPathFor(target, entryId), request, cancellationToken);
    }

    private async Task<MemoryScopeDocument> WriteAsync<T>(MemoryTarget target, HttpMethod method, string path, T payload,
        CancellationToken cancellationToken)
    {
        await _ensureGatewayReady(cancellationToken);
        using var request = new HttpRequestMessage(method, path) { Content = JsonContent.Create(payload, options: JsonOptions) };
        using var response = await _httpClient.SendAsync(request, cancellationToken);
        var document = await ReadAsync<MemoryScopeDocument>(response, cancellationToken);
        ValidateDocument(target, document, requireSourceStatus: false);
        return document;
    }

    private static string PathFor(MemoryTarget target)
    {
        target.Validate();
        return target.Scope switch
        {
            MemoryScopes.Chat => $"/api/conversations/{target.ScopeId}/memory",
            MemoryScopes.Project => $"/api/projects/{target.ScopeId}/memory",
            MemoryScopes.User => "/api/memory/user",
            _ => throw new ArgumentException("Unsupported memory scope.", nameof(target))
        };
    }

    private static string EntryPathFor(MemoryTarget target, Guid entryId)
    {
        if (entryId == Guid.Empty) throw new ArgumentException("A saved memory entry ID is required.", nameof(entryId));
        return $"{PathFor(target)}/{entryId:D}";
    }

    private static void ValidateScopeRevision(MemoryTarget target, string scope, long revision)
    {
        target.Validate();
        if (scope != target.Scope) throw new ArgumentException("Memory scope must match its target.", nameof(scope));
        if (revision is < 0 or > MemoryInputValidation.MaximumRevision)
            throw new ArgumentOutOfRangeException(nameof(revision));
    }

    private static void ValidateInput(MemoryTarget target, string scope, long revision, string content, string kind)
    {
        ValidateScopeRevision(target, scope, revision);
        var error = MemoryInputValidation.Validate(content, kind);
        if (error != MemoryInputError.None) throw new ArgumentException($"Invalid memory input: {error}.", nameof(content));
    }

    private static void ValidateDocument(MemoryTarget target, MemoryScopeDocument document, bool requireSourceStatus)
    {
        if (document.SchemaVersion != 1 || document.Scope != target.Scope || document.ScopeId != target.ScopeId ||
            document.Revision is < 0 or > MemoryInputValidation.MaximumRevision || document.Entries is null)
            throw new InvalidDataException(UiText.Get("记忆接口返回了空响应。"));
        var entryIds = new HashSet<Guid>();
        foreach (var entry in document.Entries)
        {
            if (entry is null || entry.Id == Guid.Empty || !entryIds.Add(entry.Id) || entry.Scope != document.Scope ||
                entry.ScopeId != document.ScopeId || entry.Status != "confirmed" || entry.Revision < 1 ||
                MemoryInputValidation.Validate(entry.Content, entry.Kind) != MemoryInputError.None || entry.Source is null ||
                entry.Source.Role != "user" || entry.Source.Type is not ("manual" or "user-message") ||
                (entry.Source.Type == "user-message" && (entry.Source.ConversationId is null || entry.Source.MessageId is null)) ||
                (requireSourceStatus && (entry.Active is null || entry.SourceAvailable is null || entry.SourceArchived is null)))
                throw new InvalidDataException(UiText.Get("记忆接口返回了空响应。"));
        }
    }

    private static Task<T> ReadAsync<T>(HttpResponseMessage response, CancellationToken cancellationToken) =>
        GatewayResponseReader.ReadAsync<T>(response, UiText.Get("记忆接口返回了空响应。"),
            UiText.Get("记忆接口返回 HTTP {0}。"), cancellationToken, JsonOptions);

    public void Dispose()
    {
        if (_ownsHttpClient) _httpClient.Dispose();
    }
}

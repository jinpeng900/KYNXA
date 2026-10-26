using System.Net.Http;
using System.Text.Json;
using KYNXA.Contracts;

namespace KYNXA_Desktop.Services;

/// <summary>
/// Reads a scoped usage projection without opening logs or creating a second statistics store.
/// 仅读取范围化用量投影，不打开日志，也不建立第二份统计存储。
/// </summary>
public sealed class UsageApiClient : IDisposable
{
    private static readonly JsonSerializerOptions JsonOptions = new(JsonSerializerDefaults.Web)
        { RespectRequiredConstructorParameters = true };
    private readonly HttpClient _http;
    private readonly Func<CancellationToken, Task> _ensureReady;
    private readonly bool _ownsHttp;

    public UsageApiClient() : this(GatewayHttpClient.Create(ModelGatewayService.Address, Timeout.InfiniteTimeSpan),
        ModelGatewayService.EnsureReadyAsync, ownsHttp: true) { }

    public UsageApiClient(HttpClient http, Func<CancellationToken, Task>? ensureReady = null, bool ownsHttp = false)
    { _http = http; _ensureReady = ensureReady ?? (_ => Task.CompletedTask); _ownsHttp = ownsHttp; }

    public async Task<ConversationUsageSnapshot> ReadConversationAsync(Guid conversationId,
        CancellationToken cancellationToken = default)
    {
        if (conversationId == Guid.Empty) throw new ArgumentException("A saved conversation identity is required.", nameof(conversationId));
        using var deadline = CancellationTokenSource.CreateLinkedTokenSource(cancellationToken);
        deadline.CancelAfter(TimeSpan.FromSeconds(5));
        await _ensureReady(deadline.Token);
        using var response = await _http.GetAsync($"/api/usage/summary?conversationId={conversationId:D}",
            HttpCompletionOption.ResponseHeadersRead, deadline.Token);
        var result = await GatewayResponseReader.ReadAsync<ConversationUsageSnapshot>(response,
            UiText.Get("用量接口返回了空响应。"), UiText.Get("用量接口返回 HTTP {0}。"), deadline.Token, JsonOptions);
        if (result.Scope != "conversation" || result.ConversationId != conversationId || result.ProjectId == Guid.Empty
            || result.KnownTokenRequests > result.ModelRequests || result.KnownCacheRequests > result.ModelRequests
            || result.CacheReadTokens > result.CacheInputTokens || result.UpdatedAtMs < 0
            || result.CacheHitRate is double rate && (!double.IsFinite(rate) || rate < 0 || rate > 1
                || result.KnownCacheRequests == 0 || result.CacheInputTokens == 0))
            throw new InvalidDataException(UiText.Get("用量接口返回了无效响应。"));
        return result;
    }

    public void Dispose() { if (_ownsHttp) _http.Dispose(); }
}

using System.Net.Http.Json;
using System.Text.Json;

namespace KYNXA_Desktop.Services;

/// <summary>
/// Reads authorized previews and attaches the isolated browser to the Rust tool session.
/// 读取授权预览，并将独立浏览器接入 Rust 工具会话；不直接读取工作区文件。
/// </summary>
public sealed class WorkspacePreviewApiClient : IDisposable
{
    private static readonly JsonSerializerOptions JsonOptions = new(JsonSerializerDefaults.Web);
    private readonly HttpClient _http;
    private readonly Func<CancellationToken, Task> _ensureReady;
    private readonly bool _ownsHttp;

    public WorkspacePreviewApiClient() : this(new HttpClient
    {
        BaseAddress = ModelGatewayService.Address, Timeout = Timeout.InfiniteTimeSpan
    }, ModelGatewayService.EnsureReadyAsync, true) { }

    public WorkspacePreviewApiClient(HttpClient http, Func<CancellationToken, Task>? ensureReady = null, bool ownsHttp = false)
    {
        _http = http; _ensureReady = ensureReady ?? (_ => Task.CompletedTask); _ownsHttp = ownsHttp;
    }

    public async Task<WorkspacePreviewTarget> ReadAsync(Guid conversationId, string path, int offset = 0, bool userSelected = false,
        CancellationToken cancellationToken = default)
    {
        using var deadline = CancellationTokenSource.CreateLinkedTokenSource(cancellationToken);
        deadline.CancelAfter(TimeSpan.FromSeconds(90));
        await _ensureReady(deadline.Token);
        using var response = await _http.PostAsJsonAsync("/agent/workspace/preview",
            new { conversationId, path, offset, maxChars = 64000, userSelected }, JsonOptions, deadline.Token);
        return await GatewayResponseReader.ReadAsync<WorkspacePreviewTarget>(response,
            UiText.Get("文件预览返回为空。"), UiText.Get("文件预览请求失败（HTTP {0}）。"), deadline.Token, JsonOptions);
    }

    public Uri ContentAddress(string contentUrl)
    {
        var root = _http.BaseAddress ?? ModelGatewayService.Address;
        var address = new Uri(root, contentUrl);
        if (address.Scheme != root.Scheme || address.Host != root.Host || address.Port != root.Port
            || !address.AbsolutePath.StartsWith("/agent/workspace/", StringComparison.Ordinal))
            throw new InvalidDataException(UiText.Get("文件预览地址无效。"));
        return address;
    }

    public async Task<WorkspacePreviewTarget> ReadChangesAsync(Guid conversationId, CancellationToken cancellationToken = default)
    {
        using var deadline = CancellationTokenSource.CreateLinkedTokenSource(cancellationToken);
        deadline.CancelAfter(TimeSpan.FromSeconds(45));
        await _ensureReady(deadline.Token);
        using var response = await _http.GetAsync($"/api/agent/workspace/changes?conversationId={conversationId:D}", deadline.Token);
        return await GatewayResponseReader.ReadAsync<WorkspacePreviewTarget>(response,
            UiText.Get("工作区变更返回为空。"), UiText.Get("工作区变更读取失败（HTTP {0}）。"), deadline.Token, JsonOptions);
    }

    public async Task<string> AttachBrowserAsync(Guid conversationId, string cdpEndpoint, string targetId,
        CancellationToken cancellationToken = default)
    {
        using var deadline = CancellationTokenSource.CreateLinkedTokenSource(cancellationToken);
        deadline.CancelAfter(TimeSpan.FromSeconds(15));
        await _ensureReady(deadline.Token);
        using var response = await _http.PostAsJsonAsync("/api/agent/workspace/browser",
            new { conversationId, cdpEndpoint, targetId }, JsonOptions, deadline.Token);
        using var result = await GatewayResponseReader.ReadAsync<JsonDocument>(response,
            UiText.Get("浏览器接入返回为空。"), UiText.Get("浏览器接入失败（HTTP {0}）。"), deadline.Token, JsonOptions);
        return result.RootElement.GetProperty("sessionId").GetString()
            ?? throw new InvalidDataException(UiText.Get("浏览器会话标识为空。"));
    }

    public async Task DetachBrowserAsync(Guid conversationId, string sessionId)
    {
        using var deadline = new CancellationTokenSource(TimeSpan.FromSeconds(5));
        using var request = new HttpRequestMessage(HttpMethod.Delete, "/api/agent/workspace/browser")
        { Content = JsonContent.Create(new { conversationId, sessionId }, options: JsonOptions) };
        using var response = await _http.SendAsync(request, deadline.Token);
        await GatewayResponseReader.EnsureSuccessAsync(response, UiText.Get("浏览器断开失败（HTTP {0}）。"), deadline.Token);
    }

    public void Dispose() { if (_ownsHttp) _http.Dispose(); }
}

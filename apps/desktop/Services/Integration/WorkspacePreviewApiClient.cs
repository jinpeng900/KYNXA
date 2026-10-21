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
        CancellationToken cancellationToken = default, Guid? mountId = null, string? representation = null)
    {
        using var deadline = CancellationTokenSource.CreateLinkedTokenSource(cancellationToken);
        bool office = System.IO.Path.GetExtension(path).ToLowerInvariant() is ".doc" or ".docx" or ".ppt" or ".pptx"
            or ".xls" or ".xlsx" or ".odt" or ".odp" or ".ods" or ".rtf";
        deadline.CancelAfter(TimeSpan.FromSeconds(office ? 150 : 90));
        await _ensureReady(deadline.Token);
        using var response = await _http.PostAsJsonAsync("/agent/workspace/preview",
            new { conversationId, path, offset, maxChars = 64000, userSelected, mountId, representation }, JsonOptions, deadline.Token);
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

    /// <summary>
    /// Loads an already authorized original for the local document renderer, with a bounded response body.
    /// 为本地文档渲染器读取已授权原件，限制响应体大小，不向页面开放任意网关地址。
    /// </summary>
    public async Task<byte[]> ReadDocumentContentAsync(Uri address, CancellationToken cancellationToken)
    {
        // Match the gateway's source limit; each format separately checks expanded package capacity.
        // 原件上限与网关保持一致；各格式再单独核验解包容量，不沿用 DOCX 限额拦截演示稿和表格。
        const int maximumBytes = 256 * 1024 * 1024;
        var root = _http.BaseAddress ?? ModelGatewayService.Address;
        string[] segments = address.AbsolutePath.Split('/', StringSplitOptions.RemoveEmptyEntries);
        bool attachment = segments is ["api", "conversations", var conversationId, "attachments", var attachmentId, "content"]
            && Guid.TryParse(conversationId, out _) && Guid.TryParse(attachmentId, out _);
        if (address.Scheme != root.Scheme || address.Host != root.Host || address.Port != root.Port
            || address.AbsolutePath != "/agent/workspace/content" && !attachment)
            throw new InvalidDataException(UiText.Get("文件预览地址无效。"));
        using var deadline = CancellationTokenSource.CreateLinkedTokenSource(cancellationToken);
        deadline.CancelAfter(TimeSpan.FromSeconds(45));
        using var response = await _http.GetAsync(address, HttpCompletionOption.ResponseHeadersRead, deadline.Token);
        await GatewayResponseReader.EnsureSuccessAsync(response, UiText.Get("文件内容读取失败（HTTP {0}）。"), deadline.Token);
        if (response.Content.Headers.ContentLength > maximumBytes)
            throw new InvalidDataException(UiText.Get("文档较大，正在改用页面预览。"));
        await using var source = await response.Content.ReadAsStreamAsync(deadline.Token);
        using var content = new MemoryStream();
        byte[] buffer = new byte[65536];
        int count;
        while ((count = await source.ReadAsync(buffer, deadline.Token)) != 0)
        {
            if (content.Length + count > maximumBytes)
                throw new InvalidDataException(UiText.Get("文档较大，正在改用页面预览。"));
            content.Write(buffer, 0, count);
        }
        return content.ToArray();
    }

    public async Task<WorkspacePreviewTarget> ReadChangesAsync(Guid conversationId, CancellationToken cancellationToken = default, Guid? mountId = null)
    {
        using var deadline = CancellationTokenSource.CreateLinkedTokenSource(cancellationToken);
        deadline.CancelAfter(TimeSpan.FromSeconds(45));
        await _ensureReady(deadline.Token);
        string query = $"/api/agent/workspace/changes?conversationId={conversationId:D}";
        if (mountId is { } id) query += $"&mountId={id:D}";
        using var response = await _http.GetAsync(query, deadline.Token);
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

using System.Net.Http.Json;
using System.Text.Json;

namespace KYNXA_Desktop.Services;

public sealed record ChromeWorkspacePage(string SessionId, string TabId, int ProcessId, string? Url = null,
    long? WindowId = null, string? Title = null, ChromeWorkspaceWindowBounds? WindowBounds = null,
    ChromeWorkspaceViewport? Viewport = null);
public sealed record ChromeWorkspaceWindowBounds(int Left, int Top, int Width, int Height);
public sealed record ChromeWorkspaceViewport(double Width, double Height, double DeviceScaleFactor);
public sealed record ChromeWorkspaceState(string? Url, string? Title, bool CanGoBack, bool CanGoForward,
    bool NeedsHumanVerification = false, string? AlternativeSearchUrl = null,
    long? WindowId = null, ChromeWorkspaceWindowBounds? WindowBounds = null, ChromeWorkspaceViewport? Viewport = null);

/// <summary>
/// Rust owns Chrome lifecycle; the client sends navigation and the viewport relays input.
/// Rust 管理 Chrome 生命周期；客户端提交导航，视口转交输入，不另记进程。
/// </summary>
public sealed class ChromeWorkspaceApiClient : IDisposable
{
    private static readonly JsonSerializerOptions JsonOptions = new(JsonSerializerDefaults.Web);
    private readonly HttpClient _http = GatewayHttpClient.Create(ModelGatewayService.Address, Timeout.InfiniteTimeSpan);

    public async Task<ChromeWorkspacePage> OpenAsync(Guid conversationId, Uri address, CancellationToken token)
    {
        using var deadline = CancellationTokenSource.CreateLinkedTokenSource(token);
        deadline.CancelAfter(TimeSpan.FromSeconds(30));
        await ModelGatewayService.EnsureReadyAsync(deadline.Token);
        using var response = await _http.PostAsJsonAsync("/api/agent/workspace/chrome", new { conversationId, url = address.AbsoluteUri }, JsonOptions, deadline.Token);
        return await GatewayResponseReader.ReadAsync<ChromeWorkspacePage>(response,
            UiText.Get("浏览器接入返回为空。"), UiText.Get("浏览器接入失败（HTTP {0}）。"), deadline.Token, JsonOptions);
    }

    public async Task<ChromeWorkspaceState> ActAsync(Guid conversationId, string sessionId, string action,
        Uri? address = null, CancellationToken token = default)
    {
        using var deadline = CancellationTokenSource.CreateLinkedTokenSource(token);
        deadline.CancelAfter(TimeSpan.FromSeconds(15));
        using var response = await _http.PostAsJsonAsync("/api/agent/workspace/chrome/action",
            new { conversationId, sessionId, action, url = address?.AbsoluteUri }, JsonOptions, deadline.Token);
        return await GatewayResponseReader.ReadAsync<ChromeWorkspaceState>(response,
            UiText.Get("浏览器接入返回为空。"), UiText.Get("浏览器接入失败（HTTP {0}）。"), deadline.Token, JsonOptions);
    }

    public async Task ClearWebsiteDataAsync(Guid conversationId, CancellationToken token = default)
    {
        await ModelGatewayService.EnsureReadyAsync(token);
        using var response = await _http.PostAsJsonAsync("/api/agent/workspace/chrome/action",
            new { conversationId, action = "clearSiteData" }, JsonOptions, token);
        var receipt = await GatewayResponseReader.ReadAsync<JsonElement>(response,
            UiText.Get("浏览器接入返回为空。"), UiText.Get("浏览器接入失败（HTTP {0}）。"), token, JsonOptions);
        if (!receipt.TryGetProperty("cleared", out var cleared) || cleared.ValueKind != JsonValueKind.True)
            throw new InvalidDataException(UiText.Get("网站登录数据清除未完成。"));
    }

    public void Dispose() => _http.Dispose();
}

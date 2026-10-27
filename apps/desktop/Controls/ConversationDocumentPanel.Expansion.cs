using System.Diagnostics;
using System.Net;
using System.Text.Json;
using KYNXA_Desktop.Services;
using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Controls;
using Microsoft.Web.WebView2.Core;

namespace KYNXA_Desktop.Controls;

public sealed record WorkspaceSelection(Guid ConversationId, string Kind, string Title, string Text,
    string? ObjectId, string? Version, string Path, Guid? MountId);

public sealed partial class ConversationDocumentPanel
{
    private sealed record AuthorizedResource(Uri Address, string Version);
    private readonly Dictionary<string, Dictionary<string, AuthorizedResource>> _authorizedResources = new(StringComparer.Ordinal);
    private readonly HashSet<string> _workbenchRequests = new(StringComparer.Ordinal);
    private readonly Dictionary<string, Guid> _transcriptionOperations = new(StringComparer.Ordinal);
    private readonly WorkspaceWorkbenchApiClient _workbenchApi = new();
    private bool _assetExportPending;
    private double _previewWidth = double.NaN;

    public event EventHandler<WorkspaceSelection>? SelectionRequested;
    public event EventHandler<string>? WorkbenchActionRequested;
    public event EventHandler? WorkbenchRequested;
    public event EventHandler<Guid>? ConversationRequested;

    private void SetPreviewWidth(double width)
    {
        _previewWidth = width;
        foreach (var view in _views.Values) ApplyPreviewWidth(view);
        Post(new { type = "viewport", key = _activeKey, width = double.IsNaN(width) ? 0 : (int)width });
    }

    private void ApplyPreviewWidth(BrowserView view)
    {
        view.Browser.Width = _previewWidth;
        view.Browser.MaxWidth = double.IsNaN(_previewWidth) ? double.PositiveInfinity : _previewWidth;
        view.Browser.HorizontalAlignment = double.IsNaN(_previewWidth) ? HorizontalAlignment.Stretch : HorizontalAlignment.Center;
    }

    public void ShowWorkbench(Guid conversationId, Guid? mountId = null)
    {
        if (_disposed || conversationId == Guid.Empty) return;
        string path = "expansion-workbench:" + mountId?.ToString("D");
        string key = $"{conversationId:D}:file:{path}";
        if (_activeKey == key && !_isWebsite) return;
        SaveViewState(); RememberDocument();
        _conversationId = conversationId; _activeKey = key; _path = path; _title = "工作台";
        _requestedVersion = "1"; _mountId = mountId; _attachment = null; _attachmentMessageId = null;
        _isChanges = _isWebsite = _showSource = false; _userSelected = true; _contentAddress = null; _current = null;
        if (!_documentWork.ContainsKey(key))
        {
            _documentWork[key] = new(key, conversationId, path, _title, true, "1", false, null, null, mountId)
            {
                Preview = new() { Kind = "workbench", Title = _title, Path = path, Version = "1", MimeType = "text/html" }
            };
        }
        OpenCachedOrRead();
    }

    private (string? ContentUrl, Dictionary<string, string> Resources) PrepareContentResources(BrowserView view, DocumentWork work)
    {
        ApplyPreviewWidth(view);
        var allowlist = new Dictionary<string, AuthorizedResource>(StringComparer.Ordinal);
        var resources = new Dictionary<string, string>(StringComparer.Ordinal);
        string Add(Uri address, string version, int index)
        {
            string url = "https://" + DocumentContentHost + "/Workspace/resource/" + index
                + "?version=" + Uri.EscapeDataString(version) + "&render=" + (view.RenderId + 1);
            allowlist[url] = new(address, version);
            return url;
        }
        string? content = work.ContentAddress?.AbsoluteUri;
        if (work.Preview is { Kind: "model3d" or "audio" or "video" or "image" } preview && work.ContentAddress is { } main)
            content = Add(main, preview.Version, 0);
        int index = 1;
        foreach (var dependency in work.Preview?.Dependencies ?? [])
        {
            if (!dependency.Available || string.IsNullOrWhiteSpace(dependency.ContentUrl) || string.IsNullOrWhiteSpace(dependency.Uri)) continue;
            // Only the gateway's returned dependency projection authorizes access; the page cannot add URLs.
            Uri address = _api.ContentAddress(dependency.ContentUrl);
            string proxy = Add(address, dependency.Version, index++);
            resources[dependency.Uri] = proxy;
            if (!string.IsNullOrWhiteSpace(dependency.ResolvedUri)) resources[dependency.ResolvedUri] = proxy;
        }
        _authorizedResources[view.Key] = allowlist;
        return (content, resources);
    }

    private bool TrySupplyExpansionContent(BrowserView view, CoreWebView2WebResourceRequestedEventArgs args,
        CoreWebView2Environment environment)
    {
        if (!Uri.TryCreate(args.Request.Uri, UriKind.Absolute, out var uri) || uri.Scheme != "https"
            || uri.Host != DocumentContentHost || !uri.AbsolutePath.StartsWith("/Workspace/resource/", StringComparison.Ordinal)) return false;
        if (view.Work is not { } work || !IsLive(work) || view.Browser.CoreWebView2.Source != PreviewPage
            || !_authorizedResources.TryGetValue(view.Key, out var resources) || !resources.TryGetValue(args.Request.Uri, out var resource)
            || args.Request.Method is not ("GET" or "HEAD"))
        { args.Response = environment.CreateWebResourceResponse(null, 403, "Forbidden", ""); return true; }
        var deferral = args.GetDeferral();
        _ = SupplyExpansionContentAsync(view, work, resource, args, environment, deferral.Complete);
        return true;
    }

    private async Task SupplyExpansionContentAsync(BrowserView view, DocumentWork work, AuthorizedResource resource,
        CoreWebView2WebResourceRequestedEventArgs args, CoreWebView2Environment environment, Action complete)
    {
        try
        {
            string? range = args.Request.Headers.Contains("Range") ? args.Request.Headers.GetHeader("Range") : null;
            var content = await _api.ReadProxyContentAsync(resource.Address, range, args.Request.Method, work.Cancellation.Token);
            if (!IsLive(work) || view.Closed || !_authorizedResources.TryGetValue(view.Key, out var resources)
                || !resources.TryGetValue(args.Request.Uri, out var current) || current != resource)
            { args.Response = environment.CreateWebResourceResponse(null, 409, "Document changed", ""); return; }
            args.Response = environment.CreateWebResourceResponse(new MemoryStream(content.Bytes, writable: false).AsRandomAccessStream(),
                content.StatusCode, content.Reason, content.Headers);
        }
        catch (Exception error)
        {
            Debug.WriteLine($"Workspace resource proxy: {error.GetType().Name}");
            try { args.Response = environment.CreateWebResourceResponse(null, error is InvalidDataException ? 416 : 502, "Content unavailable", ""); }
            catch (Exception closed) { Debug.WriteLine($"Workspace resource response: {closed.GetType().Name}"); }
        }
        finally
        {
            try { complete(); }
            catch (Exception closed) { Debug.WriteLine($"Workspace resource deferral: {closed.GetType().Name}"); }
        }
    }

    private static void SendActiveState(BrowserView view, bool active)
    {
        if (view.Closed || view.Browser.CoreWebView2?.Source != PreviewPage) return;
        try { view.Browser.CoreWebView2.PostWebMessageAsJson(JsonSerializer.Serialize(new { type = "setActive", key = view.Key, active })); }
        catch (System.Runtime.InteropServices.COMException) { }
    }

    private bool TryHandleExpansionMessage(BrowserView view, JsonElement message, string? type)
    {
        if (message.TryGetProperty("renderId", out var render) && (!render.TryGetInt64(out long id) || id != view.RenderId)) return true;
        if (view.Work is not { } work || !IsLive(work)) return true;
        var payload = message.TryGetProperty("payload", out var nested) && nested.ValueKind == JsonValueKind.Object ? nested : message;
        if (type == "selection")
        {
            string? text = JsonString(payload, "text", 32768);
            if (!string.IsNullOrWhiteSpace(text)) SelectionRequested?.Invoke(this, new(work.ConversationId,
                JsonString(payload, "kind", 80) ?? work.Preview?.Kind ?? "text", JsonString(payload, "title", 256) ?? work.Title ?? work.Path,
                text, JsonString(payload, "objectId", 512), JsonString(payload, "version", 128) ?? work.Preview?.Version, work.Path, work.MountId));
            return true;
        }
        if (type is "openFile" or "editSource")
        {
            string? path = JsonString(payload, "path", 4096);
            if (!string.IsNullOrWhiteSpace(path)) FileRequested?.Invoke(this, (work.ConversationId, path, work.MountId));
            return true;
        }
        if (type == "navigateConversation" && work.Preview?.Kind == "workbench")
        {
            string? conversation = JsonString(payload, "conversationId", 36) ?? JsonString(payload, "id", 36);
            if (Guid.TryParse(conversation, out Guid target)) ConversationRequested?.Invoke(this, target);
            return true;
        }
        if (type == "workbench-action" && work.Preview?.Kind == "workbench")
        {
            string? action = JsonString(payload, "action", 80);
            if (action is "attach" or "folder" or "files" or "terminal" or "extensions" or "settings" or "keyboard" or "notifications" or "notifications-off" or "remote-start" or "remote-stop")
                WorkbenchActionRequested?.Invoke(this, action);
            return true;
        }
        if (type == "workbenchRequest" && (work.Preview?.Kind == "workbench"
            || work.Preview?.Kind is "audio" or "video" && JsonString(message, "action", 80) is "media.transcribe" or "media.cancel"))
        { _ = HandleWorkbenchRequestAsync(view, work, message.Clone()); return true; }
        if (type == "exportAsset") { _ = ExportAssetAsync(view, message.Clone()); return true; }
        if (type == "refresh") { RefreshCurrent(); return true; }
        return false;
    }

    private static string? JsonString(JsonElement value, string property, int maximum) => value.TryGetProperty(property, out var text)
        && text.ValueKind == JsonValueKind.String && text.GetString() is { } result && result.Length <= maximum ? result : null;

    private async Task HandleWorkbenchRequestAsync(BrowserView view, DocumentWork work, JsonElement message)
    {
        string? requestId = JsonString(message, "requestId", 100), action = JsonString(message, "action", 80);
        if (requestId is null) return;
        long renderId = view.RenderId;
        if (action is null)
        { SendWorkbenchResult(view, work, renderId, requestId, null, "工作台操作无效。"); return; }
        string identity = view.Key + ":" + renderId + ":" + requestId;
        // The original in-flight request will supply its receipt; duplicates never dispatch twice.
        // 同一请求沿用原回执，不再派发；取消请求保留一个独立名额。
        if (_workbenchRequests.Contains(identity)) return;
        if (_workbenchRequests.Count >= (action == "media.cancel" ? 13 : 12))
        { SendWorkbenchResult(view, work, renderId, requestId, null, "工作台正忙，请稍后重试。"); return; }
        _workbenchRequests.Add(identity);
        object? result = null;
        string? error = null;
        Guid? transcription = null;
        CancellationTokenRegistration transcriptionCancellation = default;
        bool completed = false;
        try
        {
            var payload = message.TryGetProperty("payload", out var value) ? value : JsonSerializer.SerializeToElement(new { });
            if (payload.ValueKind != JsonValueKind.Object) throw new InvalidDataException("工作台参数无效。");
            if (payload.GetRawText().Length > 1024 * 1024) throw new InvalidDataException("工作台请求过大。");
            if (action == "media.cancel")
            {
                if (_transcriptionOperations.TryGetValue(work.Key, out var operation))
                    result = await _workbenchApi.RequestAsync(work.ConversationId, work.MountId, action,
                        JsonSerializer.SerializeToElement(new { operationId = operation }), work.Cancellation.Token);
                else result = new { cancelled = false, notice = "此预览没有正在运行的转录。" };
            }
            else
            {
                if (action == "media.transcribe")
                {
                    if (work.Attachment is not null) throw new InvalidOperationException("请先把此音视频附件保存到关联工作目录，再打开该文件进行本机转录。");
                    if (_transcriptionOperations.ContainsKey(work.Key)) throw new InvalidOperationException("此预览正在转录，请先等待或取消。");
                    string language = JsonString(payload, "language", 10) ?? "auto";
                    if (language is not ("auto" or "zh" or "en")) throw new InvalidDataException("转录语言必须是自动、中文或英文。");
                    bool media = work.Preview?.Kind is "audio" or "video";
                    string? path = media ? work.Path : JsonString(payload, "path", 4096);
                    if (string.IsNullOrWhiteSpace(path)) throw new InvalidDataException("请先选择音视频文件。");
                    Guid operation = Guid.NewGuid();
                    _transcriptionOperations[work.Key] = operation; transcription = operation;
                    payload = JsonSerializer.SerializeToElement(new { path, language, operationId = operation,
                        version = media ? work.Preview?.Version : JsonString(payload, "version", 64) });
                    transcriptionCancellation = work.Cancellation.Token.Register(() =>
                    { _ = CancelTranscriptionAsync(work.ConversationId, work.MountId, operation); });
                }
                result = await _workbenchApi.RequestAsync(work.ConversationId, work.MountId, action, payload, work.Cancellation.Token);
            }
            completed = true;
        }
        catch (Exception failure) when (failure is HttpRequestException or InvalidDataException or OperationCanceledException
            or JsonException or InvalidOperationException or NotSupportedException)
        { error = failure is OperationCanceledException ? "请求已取消。" : failure.Message; }
        finally
        {
            transcriptionCancellation.Dispose();
            if (transcription is { } operation)
            {
                if (_transcriptionOperations.GetValueOrDefault(work.Key) == operation) _transcriptionOperations.Remove(work.Key);
                if (!completed) _ = CancelTranscriptionAsync(work.ConversationId, work.MountId, operation);
            }
            _workbenchRequests.Remove(identity);
        }
        SendWorkbenchResult(view, work, renderId, requestId, result, error);
    }

    private void SendWorkbenchResult(BrowserView view, DocumentWork work, long renderId, string requestId, object? result, string? error)
    {
        if (_disposed || view.Closed || !IsLive(work) || view.RenderId != renderId || view.Browser.CoreWebView2?.Source != PreviewPage) return;
        try { view.Browser.CoreWebView2.PostWebMessageAsJson(JsonSerializer.Serialize(new { type = "workbenchResult",
            key = view.Key, renderId, requestId, result, error })); }
        catch (Exception closed) when (closed is System.Runtime.InteropServices.COMException or InvalidOperationException)
        { Debug.WriteLine($"Workspace reply after view closure: {closed.GetType().Name}"); }
    }

    private static async Task CancelTranscriptionAsync(Guid conversationId, Guid? mountId, Guid operation)
    {
        try
        {
            using var cancellation = new CancellationTokenSource(TimeSpan.FromSeconds(5));
            using var client = new WorkspaceWorkbenchApiClient();
            await client.RequestAsync(conversationId, mountId, "media.cancel",
                JsonSerializer.SerializeToElement(new { operationId = operation }), cancellation.Token);
        }
        catch (Exception failure) when (failure is HttpRequestException or IOException or OperationCanceledException
            or InvalidOperationException or JsonException)
        { Debug.WriteLine($"Workspace transcription cancellation: {failure.GetType().Name}"); }
    }

    private async Task ExportAssetAsync(BrowserView view, JsonElement message)
    {
        string? filename = JsonString(message, "filename", 180), mimeType = JsonString(message, "mimeType", 120);
        string? encoding = JsonString(message, "encoding", 10), requestId = JsonString(message, "requestId", 100);
        if (filename is null || mimeType is null || encoding is null || !message.TryGetProperty("content", out var content)
            || content.ValueKind != JsonValueKind.String) return;
        long renderId = view.RenderId;
        string outcome = "error";
        string? error = null;
        if (_assetExportPending)
        {
            SendAssetExportResult(view, renderId, requestId, "error", "请先完成当前保存。");
            return;
        }
        _assetExportPending = true;
        try
        {
            var asset = await WorkspaceAssetExport.ValidateAsync(filename, mimeType, content.GetString() ?? "", encoding);
            if (_disposed || view.Closed || view.RenderId != renderId || _activeKey != view.Key) return;
            nint owner = XamlRoot is null ? 0 : Microsoft.UI.Win32Interop.GetWindowFromWindowId(XamlRoot.ContentIslandEnvironment.AppWindowId);
            outcome = (await WorkspaceAssetExport.SaveAsync(asset, owner)).ToString().ToLowerInvariant();
        }
        catch (Exception failure) when (failure is IOException or UnauthorizedAccessException or FormatException or ArgumentException
            or InvalidOperationException or NotSupportedException or System.Runtime.InteropServices.COMException or System.Xml.XmlException)
        { error = failure.Message; }
        finally { _assetExportPending = false; }
        SendAssetExportResult(view, renderId, requestId, outcome, error);
    }

    private void SendAssetExportResult(BrowserView view, long renderId, string? requestId, string outcome, string? error)
    {
        if (_disposed || view.Closed || view.RenderId != renderId || view.Browser.CoreWebView2?.Source != PreviewPage) return;
        try { view.Browser.CoreWebView2.PostWebMessageAsJson(JsonSerializer.Serialize(new { type = "exportAssetResult",
            key = view.Key, renderId, requestId, outcome, error })); }
        catch (Exception closed) when (closed is System.Runtime.InteropServices.COMException or InvalidOperationException)
        { Debug.WriteLine($"Workspace export receipt after view closure: {closed.GetType().Name}"); }
    }
}

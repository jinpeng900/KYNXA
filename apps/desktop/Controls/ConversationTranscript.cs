using System.ComponentModel;
using System.Text.Json;
using KYNXA_Desktop.Services;
using KYNXA_Desktop.ViewModels;
using Microsoft.UI;
using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Controls;
using Microsoft.Web.WebView2.Core;
using Windows.ApplicationModel.DataTransfer;
using Windows.System;

namespace KYNXA_Desktop.Controls;

/// <summary>A single document keeps browser selection continuous across the whole conversation.</summary>
public sealed class ConversationTranscript : Grid, IDisposable
{
    private const string HostName = "kynxa-transcript.local";
    private const string PageUrl = "https://" + HostName + "/Transcript/index.html";
    private readonly WebView2 _browser = new() { DefaultBackgroundColor = Colors.White };
    private readonly TextBlock _notice = new() { Text = "正在加载聊天…", Margin = new Thickness(12), TextWrapping = TextWrapping.Wrap };
    private readonly TaskCompletionSource _ready = new(TaskCreationOptions.RunContinuationsAsynchronously);
    private readonly DispatcherTimer _refresh = new() { Interval = TimeSpan.FromMilliseconds(40) };
    private readonly Dictionary<Guid, CachedHtml> _html = [];
    private IReadOnlyList<ConversationMessageViewModel> _messages = [];
    private Guid? _conversationId;
    private Task? _initialization;
    private long _revision, _generation;
    private bool _openAtBottom, _dirty, _rendering, _disposed;
    private sealed record CachedHtml(string Content, string Reasoning, bool Streaming, string Html, string ReasoningHtml);
    private sealed record Snapshot(Guid Id, string Role, string Content, string Reasoning, bool Streaming,
        string ReasoningTitle, bool Waiting, string Error, bool CanRetry);

    public Task Ready => _ready.Task;
    internal WebView2 Browser => _browser;
    public event EventHandler<Guid>? RetryRequested;

    public ConversationTranscript()
    {
        Children.Add(_browser);
        Children.Add(_notice);
        Loaded += (_, _) => Preload();
        _refresh.Tick += async (_, _) => { _refresh.Stop(); await FlushAsync(); };
    }

    public void Preload()
    {
        if (_disposed || !IsLoaded) return;
        _initialization ??= InitializeAsync();
    }

    public void ShowConversation(Guid? conversationId, IReadOnlyList<ConversationMessageViewModel> messages, bool openAtBottom = true)
    {
        if (_disposed) return;
        foreach (var message in _messages) message.PropertyChanged -= MessageChanged;
        bool changed = _conversationId != conversationId;
        _conversationId = conversationId;
        _generation++;
        _messages = messages.ToArray();
        foreach (var message in _messages) message.PropertyChanged += MessageChanged;
        _openAtBottom |= changed || openAtBottom;
        if (changed) _html.Clear();
        else
            foreach (var id in _html.Keys.Where(id => !_messages.Any(message => message.Message.Id == id)).ToArray()) _html.Remove(id);
        // Clear the previous conversation immediately, even while the next one is being parsed.
        if (changed) Post(new { type = "render", conversationId = conversationId?.ToString() ?? "", openAtBottom = true, messages = Array.Empty<object>() });
        QueueRefresh();
    }

    public void BeforeSend() => Post(new { type = "beforeSend" });
    public void ClearSelection() => Post(new { type = "clearSelection" });
    private void MessageChanged(object? sender, PropertyChangedEventArgs e) => QueueRefresh();
    private void QueueRefresh()
    {
        if (_disposed) return;
        _revision++;
        _dirty = true;
        _refresh.Start();
    }

    private async Task InitializeAsync()
    {
        try
        {
            string resources = Path.Combine(AppContext.BaseDirectory, "Resources");
            if (!File.Exists(Path.Combine(resources, "Transcript", "index.html"))) throw new FileNotFoundException("聊天显示资源缺失。");
            string cache = Path.Combine(Environment.GetFolderPath(Environment.SpecialFolder.LocalApplicationData), "KYNXA", "Cache", "TranscriptWebView2");
            var environment = await CoreWebView2Environment.CreateWithOptionsAsync(null, cache, null).AsTask().WaitAsync(TimeSpan.FromSeconds(15));
            if (_disposed) return;
            await _browser.EnsureCoreWebView2Async(environment).AsTask().WaitAsync(TimeSpan.FromSeconds(15));
            if (_disposed) return;
            var core = _browser.CoreWebView2;
            core.Settings.AreDevToolsEnabled = false;
            // Keep text editing/selection shortcuts, without browser reload or navigation keys.
            core.Settings.AreBrowserAcceleratorKeysEnabled = false;
            core.Settings.IsStatusBarEnabled = false;
            core.Settings.IsWebMessageEnabled = true;
            core.Settings.IsPasswordAutosaveEnabled = false;
            core.Settings.IsGeneralAutofillEnabled = false;
            core.SetVirtualHostNameToFolderMapping(HostName, resources, CoreWebView2HostResourceAccessKind.DenyCors);
            core.NavigationStarting += (_, e) => { if (e.Uri != PageUrl) e.Cancel = true; };
            core.NewWindowRequested += (_, e) => e.Handled = true;
            core.PermissionRequested += (_, e) => e.State = CoreWebView2PermissionState.Deny;
            core.DownloadStarting += (_, e) => e.Cancel = true;
            core.ContextMenuRequested += (_, e) =>
            {
                for (int i = e.MenuItems.Count - 1; i >= 0; i--)
                    if (e.MenuItems[i].Name is not ("copy" or "selectAll")) e.MenuItems.RemoveAt(i);
                if (e.MenuItems.Count == 0) e.Handled = true;
            };
            core.AddWebResourceRequestedFilter("*", CoreWebView2WebResourceContext.All);
            core.WebResourceRequested += (_, e) =>
            {
                if (!IsLocalResource(e.Request.Uri)) e.Response = environment.CreateWebResourceResponse(null, 403, "Forbidden", "");
            };
            core.WebMessageReceived += MessageReceived;
            core.Navigate(PageUrl);
            await _ready.Task.WaitAsync(TimeSpan.FromSeconds(15));
            _notice.Visibility = Visibility.Collapsed;
            QueueRefresh();
        }
        catch (Exception error)
        {
            if (_disposed) return;
            _notice.Text = "聊天显示未能加载，请重新打开应用。";
            _notice.Visibility = Visibility.Visible;
            _ready.TrySetException(error);
        }
    }

    private async void MessageReceived(CoreWebView2 sender, CoreWebView2WebMessageReceivedEventArgs e)
    {
        if (_disposed || e.Source != PageUrl) return;
        try
        {
            using var document = JsonDocument.Parse(e.WebMessageAsJson);
            var message = document.RootElement;
            if (!message.TryGetProperty("type", out var kind)) return;
            switch (kind.GetString())
            {
                case "ready": _ready.TrySetResult(); QueueRefresh(); break;
                case "retry":
                    if (MatchesConversation(message) && message.TryGetProperty("id", out var id) && Guid.TryParse(id.GetString(), out var parsed)
                        && _messages.Any(row => row.Message.Id == parsed && row.RetryVisibility == Visibility.Visible)) RetryRequested?.Invoke(this, parsed);
                    break;
                case "link":
                    if (MatchesConversation(message) && message.TryGetProperty("url", out var url) && Uri.TryCreate(url.GetString(), UriKind.Absolute, out var uri)
                        && uri.Scheme is "http" or "https" or "mailto") await Launcher.LaunchUriAsync(uri);
                    break;
                case "copy":
                    if (MatchesConversation(message) && message.TryGetProperty("text", out var text) && text.ValueKind == JsonValueKind.String)
                    {
                        var data = new DataPackage();
                        data.SetText(text.GetString() ?? "");
                        Clipboard.SetContent(data);
                    }
                    break;
            }
        }
        catch (Exception error) when (error is JsonException or InvalidOperationException or ArgumentException or System.Runtime.InteropServices.COMException) { }
    }

    private bool MatchesConversation(JsonElement message) => message.TryGetProperty("conversationId", out var id)
        && id.GetString() == (_conversationId?.ToString() ?? "");
    private static bool IsLocalResource(string value) => Uri.TryCreate(value, UriKind.Absolute, out var uri) && uri.Scheme == "https" && uri.Host == HostName;
    private void Post(object data)
    {
        if (_disposed || !_ready.Task.IsCompletedSuccessfully) return;
        try { _browser.CoreWebView2.PostWebMessageAsJson(JsonSerializer.Serialize(data)); }
        catch (System.Runtime.InteropServices.COMException) { }
    }

    private async Task FlushAsync()
    {
        if (_disposed || _rendering || !_dirty || !_ready.Task.IsCompletedSuccessfully) return;
        _rendering = true;
        _dirty = false;
        long revision = _revision;
        long generation = _generation;
        var conversationId = _conversationId;
        var snapshots = _messages.Select(row => new Snapshot(row.Message.Id, row.Message.Role, row.Content, row.Message.Reasoning,
            row.IsStreaming, row.Message.Reasoning.Length > 0 ? row.ReasoningTitle : "", row.IsWaiting, row.ErrorText, row.RetryVisibility == Visibility.Visible)).ToArray();
        var cache = new Dictionary<Guid, CachedHtml>(_html);
        try
        {
            // Parsing and code highlighting must not hold up the shell or its pointer/keyboard input.
            var rendered = await Task.Run(() => snapshots.Select(row =>
            {
                if (cache.TryGetValue(row.Id, out var cached) && cached.Content == row.Content && cached.Reasoning == row.Reasoning && cached.Streaming == row.Streaming)
                    return (Row: row, Cache: cached);
                string html = row.Role == "user" ? "" : TranscriptMarkdown.Render(row.Content, row.Streaming);
                string reasoning = TranscriptMarkdown.Render(row.Reasoning, row.Streaming);
                return (Row: row, Cache: new CachedHtml(row.Content, row.Reasoning, row.Streaming, html, reasoning));
            }).ToArray());
            if (_disposed || generation != _generation) return;
            // A newer stream tick is allowed to queue behind this snapshot; switching chats is not.
            foreach (var item in rendered) _html[item.Row.Id] = item.Cache;
            Post(new { type = "render", conversationId = conversationId?.ToString() ?? "", revision, openAtBottom = _openAtBottom,
                messages = rendered.Select(item => new { id = item.Row.Id, role = item.Row.Role, content = item.Row.Content,
                    html = item.Cache.Html, reasoningHtml = item.Cache.ReasoningHtml, reasoningTitle = item.Row.ReasoningTitle,
                    streaming = item.Row.Streaming, waiting = item.Row.Waiting, error = item.Row.Error, canRetry = item.Row.CanRetry }) });
            _openAtBottom = false;
            _notice.Visibility = Visibility.Collapsed;
        }
        catch (Exception error) when (error is ArgumentException or InvalidOperationException or System.Text.RegularExpressions.RegexMatchTimeoutException)
        {
            if (_disposed || generation != _generation) return;
            _notice.Text = "聊天内容暂时无法显示，请重新打开此聊天。";
            _notice.Visibility = Visibility.Visible;
        }
        finally
        {
            _rendering = false;
            if (_dirty && !_disposed) _refresh.Start();
        }
    }

    public void Dispose()
    {
        if (_disposed) return;
        _disposed = true;
        _refresh.Stop();
        foreach (var message in _messages) message.PropertyChanged -= MessageChanged;
        _messages = [];
        _html.Clear();
        _browser.Close();
    }
}

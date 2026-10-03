using System.ComponentModel;
using System.Text.Json;
using KYNXA.Contracts;
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
    private readonly TextBlock _notice = new() { Text = UiText.Get("正在加载聊天…"), Margin = new Thickness(12), TextWrapping = TextWrapping.Wrap };
    private readonly TaskCompletionSource _ready = new(TaskCreationOptions.RunContinuationsAsynchronously);
    private readonly DispatcherTimer _refresh = new() { Interval = TimeSpan.FromMilliseconds(40) };
    private readonly Dictionary<Guid, CachedHtml> _html = [];
    private readonly LinkedList<Guid> _cacheOrder = [];
    private readonly Dictionary<Guid, LinkedListNode<Guid>> _cacheNodes = [];
    private const int MaximumCachedMessages = 1024;
    private const long MaximumCacheCharacters = 8 * 1024 * 1024;
    private long _cacheCharacters;
    private IReadOnlyList<ConversationMessageViewModel> _messages = [];
    private Guid? _conversationId;
    private Task? _initialization;
    private string _noticeKey = "正在加载聊天…";
    private long _revision, _generation;
    private bool _openAtBottom, _dirty, _rendering, _disposed;
    private sealed record CachedHtml(string Role, string Content, string Reasoning, bool Streaming, string Html, string ReasoningHtml)
    {
        public long Size => (long)Content.Length + Reasoning.Length + Html.Length + ReasoningHtml.Length;
    }
    private sealed record Snapshot(Guid Id, string Role, string Content, string Reasoning, bool Streaming,
        string? ReasoningState, long ReasoningSeconds, bool Waiting, string Status, string Error, bool CanRetry,
        ToolActivity[] ToolActivities);

    public Task Ready => _ready.Task;
    internal WebView2 Browser => _browser;
    public event EventHandler<Guid>? RetryRequested;

    public ConversationTranscript()
    {
        Children.Add(_browser);
        Children.Add(_notice);
        Loaded += (_, _) => Preload();
        _refresh.Tick += async (_, _) => { _refresh.Stop(); await FlushAsync(); };
        UiText.LanguageChanged += LanguageChanged;
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
        // The browser can restore a recently visited conversation while updated content is parsed.
        if (changed) Post(new { type = "openConversation", conversationId = conversationId?.ToString() ?? "" });
        QueueRefresh();
        // A navigation should not wait for the 40 ms stream batching timer.
        if (!_rendering && _ready.Task.IsCompletedSuccessfully) { _refresh.Stop(); _ = FlushAsync(); }
    }

    public void BeforeSend() => Post(new { type = "beforeSend" });
    public void ClearSelection() => Post(new { type = "clearSelection" });
    private void LanguageChanged(object? sender, EventArgs e)
    {
        if (_disposed) return;
        if (DispatcherQueue.HasThreadAccess) RefreshLanguage();
        else DispatcherQueue.TryEnqueue(RefreshLanguage);
    }

    private void RefreshLanguage()
    {
        if (_disposed) return;
        _notice.Text = UiText.Get(_noticeKey);
        // The browser retains language-neutral metadata for active and cached rows.
        // Do not queue a transcript render: that would also touch selection and scrolling.
        Post(new
        {
            type = "initializeUi", language = UiText.Language,
            strings = new
            {
                conversation = UiText.Get("对话"), transcript = UiText.Get("聊天记录"),
                copy = UiText.Get("复制"), copyMessage = UiText.Get("复制整条消息"), retry = UiText.Get("重试"),
                reasoning = UiText.Get("思考过程"), thinking = UiText.Get("正在思考…"),
                reasoningDuration = UiText.Get("思考过程 · {0} 秒"), stopped = UiText.Get("已停止生成"),
                interrupted = UiText.Get("回复中断，请重试。"),
                replying = UiText.Get("正在回复…"), generating = UiText.Get("正在生成"),
                toolActivities = UiText.Get("工具活动"), toolRunning = UiText.Get("执行中"),
                toolCompleted = UiText.Get("已完成"), toolError = UiText.Get("工具失败"),
                toolDenied = UiText.Get("已拒绝"), toolApproval = UiText.Get("等待批准")
            }
        });
    }

    private void SetNotice(string key)
    {
        _noticeKey = key;
        _notice.Text = UiText.Get(key);
    }

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
            if (!File.Exists(Path.Combine(resources, "Transcript", "index.html"))) throw new FileNotFoundException(UiText.Get("聊天显示资源缺失。"));
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
            SetNotice("聊天显示未能加载，请重新打开应用。");
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
                case "ready":
                    _ready.TrySetResult();
                    RefreshLanguage();
                    QueueRefresh();
                    break;
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
            row.IsStreaming, row.Message.Reasoning.Length > 0 ? (row.IsThinking ? "thinking" : "finished") : null,
            row.ReasoningSeconds, row.IsWaiting, row.Message.Status, row.Message.Error, row.RetryVisibility == Visibility.Visible,
            row.ToolActivities.ToArray())).ToArray();
        try
        {
            var rendered = new (Snapshot Row, CachedHtml? Cache)[snapshots.Length];
            var missing = new List<int>();
            for (int index = 0; index < snapshots.Length; index++)
            {
                var row = snapshots[index];
                _html.TryGetValue(row.Id, out var cached);
                bool hit = cached is not null && cached.Role == row.Role && cached.Content == row.Content &&
                    cached.Reasoning == row.Reasoning && cached.Streaming == row.Streaming;
                rendered[index] = (row, hit ? cached : null);
                if (!hit) missing.Add(index);
            }
            // Cached navigation can post immediately. Parse only changed messages off the UI thread.
            if (missing.Count > 0) await Task.Run(() =>
            {
                foreach (int index in missing)
                {
                    // Rapid navigation can abandon a large transcript between messages.
                    if (generation != Volatile.Read(ref _generation)) return;
                    var row = snapshots[index];
                    string html = row.Role == "user" ? "" : TranscriptMarkdown.Render(row.Content, row.Streaming);
                    string reasoning = TranscriptMarkdown.Render(row.Reasoning, row.Streaming);
                    rendered[index] = (row, new CachedHtml(row.Role, row.Content, row.Reasoning, row.Streaming, html, reasoning));
                }
            });
            if (_disposed || generation != _generation) return;
            // A newer stream tick is allowed to queue behind this snapshot; switching chats is not.
            foreach (var item in rendered) CacheHtml(item.Row.Id, item.Cache!);
            Post(new { type = "render", conversationId = conversationId?.ToString() ?? "", revision, openAtBottom = _openAtBottom,
                messages = rendered.Select(item => new { id = item.Row.Id, role = item.Row.Role, content = item.Row.Content,
                    html = item.Cache!.Html, reasoningHtml = item.Cache.ReasoningHtml, reasoningState = item.Row.ReasoningState,
                    reasoningSeconds = item.Row.ReasoningSeconds, status = item.Row.Status,
                    streaming = item.Row.Streaming, waiting = item.Row.Waiting, error = item.Row.Error, canRetry = item.Row.CanRetry,
                    toolActivities = item.Row.ToolActivities.Select(tool => new { toolCallId = tool.ToolCallId, name = tool.Name,
                        arguments = tool.Arguments, status = tool.Status, summary = tool.Summary, result = tool.Result,
                        approvalId = tool.ApprovalId, outsideWorkspace = tool.OutsideWorkspace, sandbox = tool.Sandbox,
                        workspaceRoot = tool.WorkspaceRoot }) }) });
            _openAtBottom = false;
            _notice.Visibility = Visibility.Collapsed;
        }
        catch (Exception error) when (error is ArgumentException or InvalidOperationException or System.Text.RegularExpressions.RegexMatchTimeoutException)
        {
            if (_disposed || generation != _generation) return;
            SetNotice("聊天内容暂时无法显示，请重新打开此聊天。");
            _notice.Visibility = Visibility.Visible;
        }
        finally
        {
            _rendering = false;
            if (_dirty && !_disposed) _refresh.Start();
        }
    }

    private void CacheHtml(Guid id, CachedHtml value)
    {
        if (_html.Remove(id, out var previous)) _cacheCharacters -= previous.Size;
        if (_cacheNodes.Remove(id, out var node)) _cacheOrder.Remove(node);
        if (value.Size > MaximumCacheCharacters) return;
        while (_html.Count > 0 && (_html.Count >= MaximumCachedMessages || _cacheCharacters + value.Size > MaximumCacheCharacters))
        {
            Guid oldest = _cacheOrder.First!.Value;
            _cacheCharacters -= _html[oldest].Size;
            _html.Remove(oldest);
            _cacheNodes.Remove(oldest);
            _cacheOrder.RemoveFirst();
        }
        _html.Add(id, value);
        _cacheNodes.Add(id, _cacheOrder.AddLast(id));
        _cacheCharacters += value.Size;
    }

    public void Dispose()
    {
        if (_disposed) return;
        _disposed = true;
        UiText.LanguageChanged -= LanguageChanged;
        _refresh.Stop();
        foreach (var message in _messages) message.PropertyChanged -= MessageChanged;
        _messages = [];
        _html.Clear();
        _cacheOrder.Clear();
        _cacheNodes.Clear();
        _cacheCharacters = 0;
        _browser.Close();
    }
}

using System.Text;
using System.Text.Json;
using KYNXA_Desktop.Services;
using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Automation;
using Microsoft.UI.Xaml.Controls;
using Microsoft.UI.Xaml.Media;
using Microsoft.Web.WebView2.Core;
using Windows.ApplicationModel.DataTransfer;

namespace KYNXA_Desktop.Controls;

/// <summary>
/// Conversation terminals retain their PTY and renderer when hidden; closing the tab releases both.
/// 切换标签保留聊天终端与渲染状态；关闭标签才释放对应的伪终端和视图。
/// </summary>
public sealed class ConversationWorkspaceTerminal : Grid, IDisposable
{
    private const string TerminalHost = "kynxa-terminal.local";
    private const string TerminalPage = "https://" + TerminalHost + "/Terminal/index.html";
    private static Task<CoreWebView2Environment>? _environment;
    private readonly WorkspaceTerminalApiClient _api = new();
    private readonly Dictionary<(Guid ConversationId, string TabKey), TerminalView> _views = [];
    private readonly Dictionary<(Guid ConversationId, string TabKey), Task> _closing = [];
    private readonly Grid _viewport = new();
    private readonly TextBlock _notice = new() { FontSize = 12, TextWrapping = TextWrapping.Wrap,
        VerticalAlignment = VerticalAlignment.Center };
    private readonly Button _retry = new() { Content = new FontIcon { Glyph = "\uE72C", FontSize = 12 },
        Padding = new Thickness(5), MinWidth = 24, MinHeight = 24, CornerRadius = new CornerRadius(6) };
    private readonly Button _newSession = new() { Content = new FontIcon { Glyph = "\uE710", FontSize = 12 },
        Padding = new Thickness(5), MinWidth = 24, MinHeight = 24, CornerRadius = new CornerRadius(6) };
    private readonly Grid _status = new() { Padding = new Thickness(8, 0, 4, 0), MinHeight = 24, ColumnSpacing = 5 };
    private readonly TaskCompletionSource _loaded = new(TaskCreationOptions.RunContinuationsAsynchronously);
    private TerminalView? _current;
    private bool _enabled, _disposed;

    public event EventHandler? NewSessionRequested;

    private sealed class TerminalView(Guid conversationId, string tabKey)
    {
        public Guid ConversationId { get; } = conversationId;
        public string TabKey { get; } = tabKey;
        public WebView2 Browser { get; } = new() { Visibility = Visibility.Collapsed };
        public CancellationTokenSource Lifetime { get; } = new();
        public TaskCompletionSource Ready { get; } = new(TaskCreationOptions.RunContinuationsAsynchronously);
        public Task? Startup { get; set; }
        public Task? PreviousClose { get; init; }
        public CancellationTokenSource? Reading { get; set; }
        public string? SessionId { get; set; }
        public ulong Cursor { get; set; }
        public int Cols { get; set; } = 80;
        public int Rows { get; set; } = 24;
        public int SentCols { get; set; } = 80;
        public int SentRows { get; set; } = 24;
        public bool Closed { get; set; }
        public bool Active { get; set; }
        public bool Exited { get; set; }
        public bool ReadRunning { get; set; }
        public bool Writing { get; set; }
        public bool Resizing { get; set; }
        public StringBuilder Input { get; } = new();
        public long OutputId { get; set; }
        public TaskCompletionSource? Rendered { get; set; }
        public string? Notice { get; set; }
    }

    public ConversationWorkspaceTerminal()
    {
        CornerRadius = new CornerRadius(9);
        Background = (Brush)Application.Current.Resources["KynxaReplySurfaceBrush"];
        RowDefinitions.Add(new() { Height = GridLength.Auto });
        RowDefinitions.Add(new() { Height = new GridLength(1, GridUnitType.Star) });
        _status.ColumnDefinitions.Add(new() { Width = new GridLength(1, GridUnitType.Star) });
        _status.ColumnDefinitions.Add(new() { Width = GridLength.Auto });
        _status.ColumnDefinitions.Add(new() { Width = GridLength.Auto });
        _notice.Foreground = (Brush)Application.Current.Resources["KynxaSecondaryTextBrush"];
        foreach (var button in new[] { _retry, _newSession })
        {
            button.Foreground = button.FocusVisualPrimaryBrush = _notice.Foreground;
            button.FocusVisualSecondaryBrush = new SolidColorBrush(Microsoft.UI.Colors.Transparent);
            if (Application.Current.Resources.TryGetValue("KynxaQuietButtonStyle", out var style) && style is Style quiet) button.Style = quiet;
        }
        _status.Children.Add(_notice); SetColumn(_retry, 1); _status.Children.Add(_retry);
        SetColumn(_newSession, 2); _status.Children.Add(_newSession);
        Children.Add(_status); SetRow(_viewport, 1); Children.Add(_viewport);
        _retry.Click += (_, _) => { if (_current is { } view) Retry(view); };
        _newSession.Click += (_, _) => NewSessionRequested?.Invoke(this, EventArgs.Empty);
        Loaded += (_, _) => _loaded.TrySetResult();
        AppearanceService.Changed += AppearanceChanged;
        UiText.LanguageChanged += LanguageChanged;
        UpdateStatus();
    }

    public void ShowConversation(Guid conversationId) => ShowSession(conversationId, $"terminal/{conversationId:D}");

    public void ShowSession(Guid conversationId, string tabKey)
    {
        if (_disposed || conversationId == Guid.Empty
            || _current is { } current && current.ConversationId == conversationId && current.TabKey == tabKey) return;
        if (_current is { } previous) Hide(previous);
        var key = (ConversationId: conversationId, TabKey: tabKey);
        if (!_views.TryGetValue(key, out var view))
        {
            _views[key] = view = new(conversationId, tabKey) { PreviousClose = _closing.GetValueOrDefault(key) };
            _viewport.Children.Add(view.Browser);
        }
        _current = view;
        UpdateStatus();
        if (_enabled) Activate(view);
    }

    public void SetPreviewEnabled(bool enabled)
    {
        if (_disposed || _enabled == enabled) return;
        _enabled = enabled;
        if (_current is not { } view) return;
        if (enabled) Activate(view); else Hide(view);
    }

    private void Activate(TerminalView view)
    {
        view.Active = true;
        view.Browser.Visibility = Visibility.Visible;
        if (view.Startup is null) view.Startup = StartAsync(view);
        else if (view.SessionId is not null)
        {
            view.Browser.CoreWebView2?.Resume();
            if (view.Browser.CoreWebView2 is { } core) core.MemoryUsageTargetLevel = CoreWebView2MemoryUsageTargetLevel.Normal;
            view.Browser.Focus(FocusState.Programmatic);
            Post(view, new { type = "focus" });
            StartReading(view);
        }
    }

    private static void Hide(TerminalView view)
    {
        view.Active = false;
        view.Reading?.Cancel();
        view.Browser.Visibility = Visibility.Collapsed;
        if (view.Browser.CoreWebView2 is { } core)
        {
            core.MemoryUsageTargetLevel = CoreWebView2MemoryUsageTargetLevel.Low;
            _ = SuspendAsync(view, core);
        }
    }

    private async Task StartAsync(TerminalView view)
    {
        try
        {
            Notice(view, "正在启动终端…");
            await _loaded.Task.WaitAsync(view.Lifetime.Token);
            Directory.CreateDirectory(Path.Combine(ApplicationPaths.WebViewDirectory, "WorkspaceTerminal"));
            _environment ??= CreateEnvironmentAsync();
            var environment = await _environment;
            if (view.Closed) return;
            await view.Browser.EnsureCoreWebView2Async(environment);
            if (view.Closed) return;
            var core = view.Browser.CoreWebView2;
            core.Settings.AreDevToolsEnabled = false; core.Settings.AreHostObjectsAllowed = false;
            core.Settings.IsStatusBarEnabled = false; core.Settings.IsWebMessageEnabled = true;
            core.Settings.IsPasswordAutosaveEnabled = false; core.Settings.IsGeneralAutofillEnabled = false;
            core.Settings.AreDefaultContextMenusEnabled = false;
            core.SetVirtualHostNameToFolderMapping(TerminalHost, Path.Combine(AppContext.BaseDirectory, "Resources"),
                CoreWebView2HostResourceAccessKind.DenyCors);
            core.NavigationStarting += (_, args) => { if (args.Uri != TerminalPage) args.Cancel = true; };
            core.NewWindowRequested += (_, args) => args.Handled = true;
            core.PermissionRequested += (_, args) => args.State = CoreWebView2PermissionState.Deny;
            core.DownloadStarting += (_, args) => args.Cancel = true;
            core.AddWebResourceRequestedFilter("*", CoreWebView2WebResourceContext.All);
            core.WebResourceRequested += (_, args) =>
            {
                if (!Uri.TryCreate(args.Request.Uri, UriKind.Absolute, out var address)
                    || address.Scheme != "https" || address.Host != TerminalHost)
                    args.Response = environment.CreateWebResourceResponse(null, 403, "Forbidden", "");
            };
            core.WebMessageReceived += (_, args) => Receive(view, args);
            core.Navigate(TerminalPage);
            await view.Ready.Task.WaitAsync(TimeSpan.FromSeconds(25), view.Lifetime.Token);
            ApplyTheme(view);
            if (view.PreviousClose is { } previousClose) await previousClose;
            if (view.Closed) return;
            // Keep the open receipt observable even if the tab closes while Rust starts the PTY.
            // Rust 正在启动伪终端时即使标签关闭，也等待真实接收回执后关闭对应会话。
            var session = await _api.OpenAsync(view.ConversationId, view.Cols, view.Rows);
            view.SessionId = session.SessionId;
            view.SentCols = session.Cols; view.SentRows = session.Rows;
            if (view.Closed) return;
            Notice(view, null);
            if (ReferenceEquals(_current, view) && _enabled) view.Browser.Focus(FocusState.Programmatic);
            Post(view, new { type = "connected" });
            QueueResize(view);
            if (ReferenceEquals(_current, view) && _enabled) StartReading(view);
        }
        catch (OperationCanceledException) when (view.Lifetime.IsCancellationRequested) { }
        catch (Exception error) when (IsConnectionError(error))
        { if (!view.Closed) Notice(view, "终端连接中断，请刷新重试。"); }
        finally { DispatcherQueue.TryEnqueue(UpdateStatus); }
    }

    private void Receive(TerminalView view, CoreWebView2WebMessageReceivedEventArgs args)
    {
        if (view.Closed || args.Source != TerminalPage) return;
        try
        {
            using var message = JsonDocument.Parse(args.WebMessageAsJson);
            var value = message.RootElement;
            switch (value.GetProperty("type").GetString())
            {
                case "ready":
                case "resize":
                    view.Cols = Math.Clamp(value.GetProperty("cols").GetInt32(), 10, 1000);
                    view.Rows = Math.Clamp(value.GetProperty("rows").GetInt32(), 2, 500);
                    view.Ready.TrySetResult(); QueueResize(view);
                    break;
                case "input":
                    if (view.SessionId is null || view.Exited) break;
                    view.Input.Append(value.GetProperty("data").GetString());
                    if (!view.Writing) { view.Writing = true; _ = WriteInputAsync(view); }
                    break;
                case "rendered":
                    if (value.GetProperty("id").GetInt64() == view.OutputId) view.Rendered?.TrySetResult();
                    break;
                case "paste": _ = PasteAsync(view); break;
                case "copy":
                    var package = new DataPackage(); package.SetText(value.GetProperty("text").GetString() ?? "");
                    Clipboard.SetContent(package);
                    break;
            }
        }
        catch (Exception error) when (error is JsonException or InvalidOperationException or System.Runtime.InteropServices.COMException) { }
    }

    private async Task WriteInputAsync(TerminalView view)
    {
        try
        {
            while (!view.Closed && !view.Exited && view.Input.Length > 0)
            {
                int count = Math.Min(view.Input.Length, 8192);
                if (count < view.Input.Length && char.IsHighSurrogate(view.Input[count - 1])) count--;
                string input = view.Input.ToString(0, count); view.Input.Remove(0, count);
                await _api.WriteAsync(view.ConversationId, view.SessionId!, input, view.Lifetime.Token);
            }
        }
        catch (Exception error) when (IsConnectionError(error))
        {
            // An unknown write is not replayed: a pasted command may already have executed.
            // 写入结果未知时不自动重放，粘贴的命令可能已经执行。
            view.Input.Clear();
            if (!view.Closed) Notice(view, "终端输入未确认，请检查当前输出。");
        }
        finally { view.Writing = false; }
    }

    private void QueueResize(TerminalView view)
    {
        if (view.Closed || view.SessionId is null || view.Resizing || view.Exited) return;
        view.Resizing = true; _ = ResizeAsync(view);
    }

    private async Task ResizeAsync(TerminalView view)
    {
        try
        {
            await Task.Delay(75, view.Lifetime.Token);
            while (!view.Closed && (view.SentCols != view.Cols || view.SentRows != view.Rows))
            {
                int cols = view.Cols, rows = view.Rows;
                await _api.ResizeAsync(view.ConversationId, view.SessionId!, cols, rows, view.Lifetime.Token);
                view.SentCols = cols; view.SentRows = rows;
            }
        }
        catch (Exception error) when (IsConnectionError(error))
        { if (!view.Closed) Notice(view, "终端连接中断，请刷新重试。"); }
        finally { view.Resizing = false; }
    }

    private void StartReading(TerminalView view)
    {
        if (view.ReadRunning || view.SessionId is null || view.Closed || view.Exited || !_enabled || !ReferenceEquals(_current, view)) return;
        view.ReadRunning = true;
        view.Reading = CancellationTokenSource.CreateLinkedTokenSource(view.Lifetime.Token);
        _ = ReadOutputAsync(view, view.Reading);
    }

    private async Task ReadOutputAsync(TerminalView view, CancellationTokenSource reading)
    {
        bool cancelled = false;
        try
        {
            while (!view.Closed && !view.Exited && _enabled && ReferenceEquals(_current, view))
            {
                var output = await _api.ReadAsync(view.ConversationId, view.SessionId!, view.Cursor, reading.Token);
                reading.Token.ThrowIfCancellationRequested();
                if (output.Truncated) Notice(view, "部分较早终端输出已移出缓存。");
                if (!string.IsNullOrEmpty(output.Output))
                {
                    view.Rendered = new(TaskCreationOptions.RunContinuationsAsynchronously);
                    Post(view, new { type = "output", data = output.Output, id = ++view.OutputId });
                    view.Cursor = output.NextCursor;
                    await view.Rendered.Task.WaitAsync(TimeSpan.FromSeconds(20), reading.Token);
                }
                view.Cursor = output.NextCursor;
                if (output.Exited && !output.HasMore)
                {
                    view.Exited = true; Post(view, new { type = "exited" }); Notice(view, "终端已退出。");
                }
                else if (string.IsNullOrEmpty(output.Output)) await Task.Delay(25, reading.Token);
            }
        }
        catch (OperationCanceledException) when (reading.IsCancellationRequested) { cancelled = true; }
        catch (GatewayApiException error) when (error.StatusCode is System.Net.HttpStatusCode.NotFound or System.Net.HttpStatusCode.Gone)
        {
            view.Exited = true; Post(view, new { type = "exited" });
            if (!view.Closed) Notice(view, "终端连接中断，请刷新重试。");
        }
        catch (Exception error) when (IsConnectionError(error))
        { if (!view.Closed) Notice(view, "终端连接中断，请刷新重试。"); }
        finally
        {
            view.ReadRunning = false; view.Reading = null; reading.Dispose();
            if (cancelled && !view.Closed && _enabled && ReferenceEquals(_current, view)) StartReading(view);
        }
    }

    private async Task PasteAsync(TerminalView view)
    {
        try
        {
            var clipboard = Clipboard.GetContent();
            if (clipboard.Contains(StandardDataFormats.Text))
            {
                string text = await clipboard.GetTextAsync();
                if (!view.Closed) Post(view, new { type = "paste", text });
            }
        }
        catch (Exception error) when (error is System.Runtime.InteropServices.COMException or InvalidOperationException) { }
    }

    private void Retry(TerminalView view)
    {
        if (view.Closed || view.Startup is { IsCompleted: false }) return;
        if (view.SessionId is not null && !view.Exited) { Notice(view, null); StartReading(view); }
        else { CloseSession(view.ConversationId, view.TabKey); ShowSession(view.ConversationId, view.TabKey); }
    }

    private static void Post(TerminalView view, object message)
    {
        if (!view.Closed) view.Browser.CoreWebView2?.PostWebMessageAsJson(JsonSerializer.Serialize(message));
    }

    private void Notice(TerminalView view, string? notice)
    {
        view.Notice = notice;
        if (ReferenceEquals(_current, view)) UpdateStatus();
    }

    private void UpdateStatus()
    {
        _notice.Text = _current?.Notice is { } notice ? UiText.Get(notice) : UiText.Get("终端");
        _retry.Visibility = _current?.Startup is { IsCompleted: true } ? Visibility.Visible : Visibility.Collapsed;
        AutomationProperties.SetName(_retry, UiText.Get("重新连接终端"));
        ToolTipService.SetToolTip(_retry, UiText.Get("重新连接终端"));
        AutomationProperties.SetName(_newSession, UiText.Get("新建终端"));
        ToolTipService.SetToolTip(_newSession, UiText.Get("新建终端"));
    }

    private void ApplyTheme(TerminalView view)
    {
        var palette = AppearanceService.Current;
        Post(view, new { type = "theme", theme = new { background = palette.Main, foreground = palette.Text,
            cursor = palette.Secondary, selectionBackground = palette.Border + "88" } });
    }

    private void AppearanceChanged(object? sender, EventArgs args)
    {
        foreach (var view in _views.Values) if (view.Ready.Task.IsCompletedSuccessfully) ApplyTheme(view);
    }

    private void LanguageChanged(object? sender, EventArgs args) => DispatcherQueue.TryEnqueue(UpdateStatus);

    public void CloseConversation(Guid conversationId)
    {
        foreach (var key in _views.Keys.Where(key => key.ConversationId == conversationId).ToArray())
            CloseSession(key.ConversationId, key.TabKey);
    }

    public void CloseSession(Guid conversationId, string tabKey)
    {
        var key = (ConversationId: conversationId, TabKey: tabKey);
        if (!_views.Remove(key, out var view)) return;
        view.Closed = true; view.Lifetime.Cancel(); view.Reading?.Cancel();
        _viewport.Children.Remove(view.Browser); view.Browser.Close();
        if (ReferenceEquals(_current, view)) _current = null;
        // Reopening a tab waits only for that tab's previous PTY, never another terminal in the chat.
        // 重开标签只等待该标签旧伪终端关闭，不阻塞同一聊天的其他终端。
        var closing = CloseViewAsync(view); _closing[key] = closing;
        _ = ObserveCloseAsync(key, closing);
        UpdateStatus();
    }

    private async Task CloseViewAsync(TerminalView view)
    {
        try
        {
            if (view.Startup is { } startup) await startup;
            if (view.PreviousClose is { } previousClose) await previousClose;
            if (view.SessionId is { } sessionId) await _api.CloseAsync(view.ConversationId, sessionId);
        }
        finally { view.Lifetime.Dispose(); }
    }

    private async Task ObserveCloseAsync((Guid ConversationId, string TabKey) key, Task closing)
    {
        try { await closing; }
        catch (Exception error) when (IsConnectionError(error)) { }
        finally { if (_closing.GetValueOrDefault(key) == closing) _closing.Remove(key); }
    }

    public async Task CloseAllAsync()
    {
        foreach (var key in _views.Keys.ToArray()) CloseSession(key.ConversationId, key.TabKey);
        await Task.WhenAll(_closing.Values.ToArray());
    }

    private static async Task<CoreWebView2Environment> CreateEnvironmentAsync() =>
        await CoreWebView2Environment.CreateWithOptionsAsync(null,
            Path.Combine(ApplicationPaths.WebViewDirectory, "WorkspaceTerminal"), null);

    private static async Task SuspendAsync(TerminalView view, CoreWebView2 core)
    {
        try
        {
            await core.TrySuspendAsync();
            // A late suspension must not freeze a terminal the user has already returned to.
            // 挂起完成时如果用户已切回终端，立即恢复，避免快速切换后停留空白。
            if (view.Active && !view.Closed) core.Resume();
        }
        catch (Exception error) when (error is InvalidOperationException or System.Runtime.InteropServices.COMException) { }
    }

    private static bool IsConnectionError(Exception error) => error is HttpRequestException or IOException or InvalidOperationException
        or OperationCanceledException or JsonException or TimeoutException or System.Runtime.InteropServices.COMException;

    public void Dispose()
    {
        if (_disposed) return;
        _disposed = true; AppearanceService.Changed -= AppearanceChanged; UiText.LanguageChanged -= LanguageChanged;
        _ = DisposeAfterCloseAsync();
    }

    private async Task DisposeAfterCloseAsync()
    {
        try { await CloseAllAsync(); }
        catch (Exception error) when (IsConnectionError(error)) { }
        finally { _api.Dispose(); }
    }
}

using System.ComponentModel;
using KYNXA.Contracts;
using KYNXA_Desktop.Services;
using KYNXA_Desktop.ViewModels;
using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Automation;
using Microsoft.UI.Xaml.Controls;
using Microsoft.UI.Xaml.Media;

namespace KYNXA_Desktop.Controls;

/// <summary>Read-only command/output display, not a persistent interactive shell.</summary>
public sealed class ConversationTerminalPanel : Grid, IDisposable
{
    private readonly TerminalOutputState _state = new();
    private readonly TextBlock _title = new() { Name = "TerminalPanelTitle", FontSize = 12, VerticalAlignment = VerticalAlignment.Center };
    private readonly TextBlock _position = new() { Name = "TerminalPanelPosition", FontSize = 11, VerticalAlignment = VerticalAlignment.Center };
    private readonly TextBlock _cwd = new() { Name = "TerminalPanelDirectory", FontSize = 12, TextTrimming = TextTrimming.CharacterEllipsis, Margin = new Thickness(10, 4, 10, 4) };
    private readonly TextBlock _status = new() { Name = "TerminalPanelStatus", FontSize = 12, TextWrapping = TextWrapping.Wrap, Margin = new Thickness(10, 5, 10, 8) };
    private readonly TextBox _command = Editor("TerminalPanelCommand");
    private readonly TextBox _output = Editor("TerminalPanelOutput");
    private readonly Button _previous = IconButton("TerminalPanelPrevious", "\uE76B");
    private readonly Button _next = IconButton("TerminalPanelNext", "\uE76C");
    private readonly Button _stop = IconButton("TerminalPanelStop", "\uE71A");
    private readonly Button _close = IconButton("TerminalPanelClose", "\uE711");
    private readonly Microsoft.UI.Dispatching.DispatcherQueueTimer _refreshTimer;
    private readonly Dictionary<ConversationMessageViewModel, ToolActivity[]> _snapshots = [];
    private readonly Dictionary<TerminalRunIdentity, ToolActivity> _receipts = [];
    private readonly Dictionary<TerminalRunIdentity, Guid> _archiveAttempts = [];
    private IReadOnlyList<ConversationMessageViewModel> _messages = [];
    private Guid? _conversationId;
    private TerminalRunIdentity? _renderedIdentity;
    private bool _previewEnabled, _disposed, _tabbedMode;
    private IAgentApi? _api;
    private CancellationTokenSource? _archiveCancellation;
    private TerminalRunIdentity? _loadingIdentity;
    private long _archiveGeneration;
    private string? _archiveNotice;

    public bool HasTerminal => _state.Selected(_conversationId) is not null;
    public bool IsPanelOpen => HasTerminal && !_state.IsClosed(_conversationId);
    public IReadOnlyList<TerminalOutputState.TerminalRun> Items => _conversationId is Guid id ? _state.Runs(id) : [];
    public TerminalOutputState.TerminalRun? SelectedRun => _state.Selected(_conversationId);
    public event EventHandler? TerminalChanged;
    public event EventHandler<TerminalRunIdentity>? StopRequested;

    public ConversationTerminalPanel()
    {
        Visibility = Visibility.Collapsed;
        RowDefinitions.Add(new() { Height = GridLength.Auto });
        RowDefinitions.Add(new() { Height = GridLength.Auto });
        RowDefinitions.Add(new() { Height = GridLength.Auto });
        RowDefinitions.Add(new() { Height = new GridLength(1, GridUnitType.Star) });
        RowDefinitions.Add(new() { Height = GridLength.Auto });
        var resources = Application.Current.Resources;
        if (resources.TryGetValue("KynxaSecondaryTextBrush", out var brush) && brush is Brush secondary)
            _title.Foreground = _position.Foreground = _cwd.Foreground = _status.Foreground = _previous.Foreground = _next.Foreground = _stop.Foreground = _close.Foreground = secondary;
        if (resources.TryGetValue("KynxaCompactIconButtonStyle", out var icon) && icon is Style compact)
            _previous.Style = _next.Style = _stop.Style = _close.Style = compact;
        if (resources.TryGetValue("KynxaReplySurfaceBrush", out var surface) && surface is Brush background) Background = background;
        CornerRadius = new CornerRadius(10);
        var header = new Grid { Margin = new Thickness(10, 5, 6, 3) };
        header.ColumnDefinitions.Add(new() { Width = new GridLength(1, GridUnitType.Star) });
        header.ColumnDefinitions.Add(new() { Width = GridLength.Auto });
        var navigation = new StackPanel { Orientation = Orientation.Horizontal, Spacing = 3 };
        navigation.Children.Add(_previous); navigation.Children.Add(_position); navigation.Children.Add(_next);
        navigation.Children.Add(_stop); navigation.Children.Add(_close);
        Grid.SetColumn(navigation, 1); header.Children.Add(_title); header.Children.Add(navigation); Children.Add(header);
        _command.MaxHeight = 100; _command.Margin = new Thickness(6, 0, 6, 0);
        Grid.SetRow(_command, 1); Children.Add(_command);
        Grid.SetRow(_cwd, 2); Children.Add(_cwd);
        _output.Margin = new Thickness(6, 0, 6, 0); _output.VerticalAlignment = VerticalAlignment.Stretch;
        Grid.SetRow(_output, 3); Children.Add(_output);
        Grid.SetRow(_status, 4); Children.Add(_status);
        _refreshTimer = DispatcherQueue.CreateTimer(); _refreshTimer.Interval = TimeSpan.FromMilliseconds(40); _refreshTimer.IsRepeating = false;
        _refreshTimer.Tick += (_, _) => Render();
        _output.SelectionChanged += (_, _) => { if (_output.SelectionLength == 0) ScheduleRender(); };
        _previous.Click += (_, _) => Select(-1);
        _next.Click += (_, _) => Select(1);
        _stop.Click += (_, _) => { if (_state.Selected(_conversationId) is { IsExecuting: true } run) StopRequested?.Invoke(this, run.Identity); };
        _close.Click += (_, _) => { _state.Close(_conversationId); CancelArchive(); Render(); TerminalChanged?.Invoke(this, EventArgs.Empty); };
        UiText.LanguageChanged += LanguageChanged;
        RefreshLanguage();
    }

    private static TextBox Editor(string name)
    {
        var box = new TextBox { Name = name, IsReadOnly = true, AcceptsReturn = true, TextWrapping = TextWrapping.Wrap,
            FontFamily = new FontFamily("Consolas"), FontSize = 14, BorderThickness = new Thickness(0), Padding = new Thickness(4),
            Background = new SolidColorBrush(Microsoft.UI.Colors.Transparent), MinWidth = 0, MinHeight = 0 };
        ScrollViewer.SetVerticalScrollBarVisibility(box, ScrollBarVisibility.Auto);
        ScrollViewer.SetHorizontalScrollBarVisibility(box, ScrollBarVisibility.Disabled);
        return box;
    }

    private static Button IconButton(string name, string glyph) => new()
    {
        Name = name, Content = new FontIcon { Glyph = glyph, FontSize = 10 }, Width = 26, Height = 26, MinWidth = 0, MinHeight = 0,
        Padding = new Thickness(0), BorderThickness = new Thickness(0), Background = new SolidColorBrush(Microsoft.UI.Colors.Transparent)
    };

    public void ShowConversation(Guid? conversationId, IReadOnlyList<ConversationMessageViewModel> messages)
    {
        if (_disposed) return;
        bool changed = _conversationId != conversationId;
        if (changed) { CancelArchive(); _receipts.Clear(); _archiveAttempts.Clear(); _archiveNotice = null; }
        bool same = !changed && _messages.Count == messages.Count && _messages.Zip(messages).All(pair => ReferenceEquals(pair.First, pair.Second));
        if (!same)
        {
            foreach (var message in _messages) message.PropertyChanged -= MessageChanged;
            _messages = messages.ToArray(); _snapshots.Clear();
            foreach (var message in _messages) message.PropertyChanged += MessageChanged;
        }
        _conversationId = conversationId;
        bool updated = Recover();
        if (changed || updated) { Render(); TerminalChanged?.Invoke(this, EventArgs.Empty); }
    }

    private bool Recover()
    {
        if (_conversationId is not Guid chatId) return false;
        bool changed = false;
        foreach (var message in _messages)
        {
            var tools = message.ToolActivities;
            if (_snapshots.TryGetValue(message, out var previous) && previous.Length == tools.Count && previous.Zip(tools).All(pair => ReferenceEquals(pair.First, pair.Second))) continue;
            _snapshots[message] = tools.ToArray();
            changed = true;
        }
        if (!changed) return false;
        var sources = _messages.Where(message => message.Message.Role == "assistant")
            .SelectMany(message => message.ToolActivities.Where(tool => tool.Name == "terminal.host.run")
                .Select(tool => (Message: message, Tool: tool))).TakeLast(TerminalOutputState.MaximumRunsPerConversation).ToArray();
        _receipts.Clear();
        foreach (var pair in sources)
        {
            var identity = new TerminalRunIdentity(chatId, pair.Message.Message.Id, pair.Tool.ToolCallId);
            _state.Observe(identity, pair.Tool, live: false, pair.Message.IsStreaming);
            _receipts[identity] = pair.Tool;
        }
        return true;
    }

    public void ConfigureApi(IAgentApi api) => _api = api ?? throw new ArgumentNullException(nameof(api));

    public bool Observe(Guid conversationId, Guid requestId, ToolActivity activity)
    {
        if (_disposed || activity.Name != "terminal.host.run") return false;
        bool added = _state.Observe(new(conversationId, requestId, activity.ToolCallId), activity, live: true);
        if (_conversationId != conversationId) return added;
        if (added) { CancelArchive(); _archiveNotice = null; }
        _receipts[new(conversationId, requestId, activity.ToolCallId)] = activity;
        ScheduleRender();
        if (added || activity.Status != "running") TerminalChanged?.Invoke(this, EventArgs.Empty);
        return added;
    }

    public void Append(Guid conversationId, Guid requestId, HostTerminalOutput output)
    {
        if (!_disposed && _state.Append(conversationId, requestId, output) && _conversationId == conversationId) ScheduleRender();
    }

    public void EndRequest(Guid conversationId, Guid requestId)
    {
        if (_disposed) return;
        _state.EndRequest(conversationId, requestId);
        if (_conversationId == conversationId) ScheduleRender();
    }

    public void ReopenSelected()
    {
        if (_disposed) return;
        _state.Reopen(_conversationId); Render(); TerminalChanged?.Invoke(this, EventArgs.Empty);
    }

    public bool SelectRun(TerminalRunIdentity identity)
    {
        if (_disposed || identity.ConversationId != _conversationId) return false;
        if (SelectedRun?.Identity == identity && !_state.IsClosed(_conversationId)) return true;
        int index = Items.ToList().FindIndex(run => run.Identity == identity);
        if (index < 0 || !_state.Select(identity.ConversationId, index)) return false;
        CancelArchive(); _archiveNotice = null; _state.Reopen(_conversationId); Render();
        return true;
    }

    public void SetTabbedMode(bool enabled)
    {
        if (_disposed || _tabbedMode == enabled) return;
        _tabbedMode = enabled;
        _title.Visibility = _position.Visibility = _previous.Visibility = _next.Visibility = _close.Visibility = enabled ? Visibility.Collapsed : Visibility.Visible;
        CornerRadius = new CornerRadius(enabled ? 0 : 10);
        Background = enabled ? null : Application.Current.Resources["KynxaReplySurfaceBrush"] as Brush;
        Render();
    }

    public void SetPreviewEnabled(bool enabled)
    {
        if (_disposed || _previewEnabled == enabled) return;
        _previewEnabled = enabled;
        if (_tabbedMode) Visibility = enabled && IsPanelOpen ? Visibility.Visible : Visibility.Collapsed;
        if (!enabled) { _refreshTimer.Stop(); CancelArchive(); } else Render();
    }

    private void ScheduleRender()
    {
        if (!_previewEnabled) return;
        if (!_refreshTimer.IsRunning) _refreshTimer.Start();
    }

    private void MessageChanged(object? sender, PropertyChangedEventArgs args)
    {
        if (args.PropertyName is not ("" or null or nameof(ConversationMessageViewModel.ToolActivities))) return;
        DispatcherQueue.TryEnqueue(() => { if (!_disposed && Recover()) { ScheduleRender(); TerminalChanged?.Invoke(this, EventArgs.Empty); } });
    }

    private void Select(int delta)
    {
        if (_conversationId is not Guid id || _state.Selected(id) is not { } selected) return;
        var runs = _state.Runs(id);
        int index = runs.ToList().FindIndex(run => run.Identity == selected.Identity);
        if (_state.Select(id, index + delta)) { CancelArchive(); _archiveNotice = null; Render(); }
    }

    private void Render()
    {
        if (_disposed) return;
        var run = _state.Selected(_conversationId);
        Visibility = IsPanelOpen && (!_tabbedMode || _previewEnabled) ? Visibility.Visible : Visibility.Collapsed;
        if (run is null) { _command.Text = _output.Text = _cwd.Text = _status.Text = ""; _renderedIdentity = null; return; }
        bool changed = _renderedIdentity != run.Identity;
        _renderedIdentity = run.Identity;
        _title.Text = UiText.Get("终端") + (run.Shell.Length > 0 ? " · " + run.Shell : "");
        SetEditorText(_command, run.Script, changed);
        SetEditorText(_output, run.Output, changed);
        _cwd.Text = run.Cwd;
        ToolTipService.SetToolTip(_cwd, run.Cwd);
        string key = run.Status switch { "running" => "正在执行", "approval-required" => "等待批准", "completed" => "执行完成",
            "cancelled" => "已取消", "denied" => "已拒绝", "error" => "执行失败", _ => "执行结果未确认" };
        _status.Text = UiText.Get(key) + (run.ExitCode is int code ? " · " + string.Format(UiText.Get("退出码 {0}"), code) : "") +
            (run.Truncated ? " · " + UiText.Get("仅显示最近输出") : "") +
            (_archiveNotice is not null ? " · " + UiText.Get(_archiveNotice) : "");
        _stop.IsEnabled = run.IsExecuting;
        var runs = _state.Runs(run.Identity.ConversationId);
        int index = runs.ToList().FindIndex(candidate => candidate.Identity == run.Identity);
        _position.Text = $"{index + 1}/{runs.Count}";
        _previous.IsEnabled = index > 0; _next.IsEnabled = index < runs.Count - 1;
        if (_previewEnabled && IsPanelOpen && !run.IsExecuting && !run.HasLiveOutput && !run.HasReceiptOutput &&
            _receipts.TryGetValue(run.Identity, out var receipt) && receipt.ResultRef is { } reference && IsValidReference(reference) &&
            _api is not null && _loadingIdentity != run.Identity && (!_archiveAttempts.TryGetValue(run.Identity, out var tried) || tried != reference.Id))
        {
            CancelArchive(); _loadingIdentity = run.Identity;
            _archiveNotice = "正在读取终端输出…";
            var cancellation = _archiveCancellation = new CancellationTokenSource();
            _ = LoadArchiveAsync(run.Identity, reference, ++_archiveGeneration, cancellation.Token);
        }
    }

    private static bool IsValidReference(ToolResultReference reference) => reference.Id != Guid.Empty && reference.Bytes is > 0 and <= 8 * 1024 * 1024 &&
        reference.Sha256.Length == 64 && reference.Sha256.All(Uri.IsHexDigit);

    private async Task LoadArchiveAsync(TerminalRunIdentity identity, ToolResultReference reference, long generation, CancellationToken cancellationToken)
    {
        try
        {
            var response = await _api!.GetToolResultAsync(identity.ConversationId, reference, cancellationToken);
            cancellationToken.ThrowIfCancellationRequested();
            if (_disposed || generation != _archiveGeneration || _conversationId != identity.ConversationId ||
                _state.Selected(_conversationId)?.Identity != identity || !_receipts.TryGetValue(identity, out var tool) || tool.ResultRef != reference) return;
            _archiveAttempts[identity] = reference.Id;
            _archiveNotice = _state.ApplyArchive(identity, response.Result) ? null : "完整输出保存在工具记录中";
        }
        catch (OperationCanceledException) when (cancellationToken.IsCancellationRequested) { }
        catch (Exception error) when (error is HttpRequestException or IOException or UnauthorizedAccessException or System.Text.Json.JsonException or InvalidOperationException or OperationCanceledException)
        {
            if (!_disposed && generation == _archiveGeneration) { _archiveAttempts[identity] = reference.Id; _archiveNotice = "终端输出读取失败"; }
        }
        finally
        {
            if (!_disposed && generation == _archiveGeneration)
            {
                _loadingIdentity = null; _archiveCancellation?.Dispose(); _archiveCancellation = null;
                Render();
            }
        }
    }

    private void CancelArchive()
    {
        ++_archiveGeneration;
        _archiveCancellation?.Cancel(); _archiveCancellation?.Dispose(); _archiveCancellation = null; _loadingIdentity = null;
        if (_archiveNotice == "正在读取终端输出…") _archiveNotice = null;
    }

    private static void SetEditorText(TextBox editor, string text, bool identityChanged)
    {
        if (editor.Text == text || !identityChanged && editor.SelectionLength > 0) return;
        int start = editor.SelectionStart, length = editor.SelectionLength;
        string previous = editor.Text;
        var scroll = FindScrollViewer(editor);
        bool atBottom = identityChanged || scroll is null || scroll.ScrollableHeight - scroll.VerticalOffset < 24;
        double offset = scroll?.VerticalOffset ?? 0;
        editor.Text = text;
        if (scroll is not null && length == 0) editor.DispatcherQueue.TryEnqueue(() => scroll.ChangeView(null, atBottom ? scroll.ScrollableHeight : offset, null, true));
    }

    private static ScrollViewer? FindScrollViewer(DependencyObject parent)
    {
        if (parent is ScrollViewer scroll) return scroll;
        for (int index = 0; index < VisualTreeHelper.GetChildrenCount(parent); index++)
            if (FindScrollViewer(VisualTreeHelper.GetChild(parent, index)) is { } child) return child;
        return null;
    }

    private void LanguageChanged(object? sender, EventArgs args) => DispatcherQueue.TryEnqueue(() => { if (!_disposed) { RefreshLanguage(); Render(); } });

    private void RefreshLanguage()
    {
        Label(_previous, "上一条命令"); Label(_next, "下一条命令"); Label(_stop, "停止执行"); Label(_close, "关闭终端面板");
        AutomationProperties.SetName(_command, UiText.Get("执行命令")); AutomationProperties.SetName(_output, UiText.Get("终端输出"));
        ToolTipService.SetToolTip(_stop, UiText.Get("停止此聊天的当前任务"));
    }

    private static void Label(Button button, string key)
    {
        AutomationProperties.SetName(button, UiText.Get(key)); ToolTipService.SetToolTip(button, UiText.Get(key));
    }

    public void Dispose()
    {
        if (_disposed) return;
        _disposed = true; _refreshTimer.Stop(); CancelArchive(); UiText.LanguageChanged -= LanguageChanged;
        foreach (var message in _messages) message.PropertyChanged -= MessageChanged;
        _messages = []; _snapshots.Clear(); _receipts.Clear(); _archiveAttempts.Clear(); _state.Clear();
    }
}

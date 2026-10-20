using KYNXA_Desktop.Services;
using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Automation;
using Microsoft.UI.Xaml.Controls;
using Microsoft.UI.Xaml.Media;

namespace KYNXA_Desktop.Controls;

/// <summary>
/// Mirrors one conversation work directory for the right pane; entries come from the gateway and file bytes stay
/// behind the existing preview and attachment endpoints. Rows are drawn directly so no data template or node
/// conversion can leak framework type names into the UI. Polling stops whenever the pane is not showing.
/// 为右栏镜像一个会话的工作目录；条目来自网关，文件字节仍由既有预览与附件接口提供。
/// 行控件直接构建，避免数据模板或节点转换把框架类型名显示到界面。面板不显示时停止轮询。
/// </summary>
public sealed class ConversationWorkspaceTree : Grid, IDisposable
{
    private const string FolderGlyph = "\uE8B7";
    private const string FileGlyph = "\uE8A5";
    private const string CollapsedGlyph = "\uE76C";
    private const string ExpandedGlyph = "\uE70D";
    private const string RefreshGlyph = "\uE72C";

    private readonly WorkspaceTreeApiClient _client = new();
    private readonly TextBlock _title = new() { FontSize = 12, FontWeight = Microsoft.UI.Text.FontWeights.SemiBold,
        TextTrimming = TextTrimming.CharacterEllipsis, VerticalAlignment = VerticalAlignment.Center };
    private readonly TextBlock _hint = new() { FontSize = 11, TextTrimming = TextTrimming.CharacterEllipsis,
        TextWrapping = TextWrapping.Wrap, MaxLines = 2 };
    private readonly Button _back = new() { Name = "WorkspaceTreeBack", MinWidth = 0, MinHeight = 0,
        Padding = new Thickness(5, 3, 5, 3), Content = new FontIcon { Glyph = "\uE72B", FontSize = 12 } };
    private readonly Button _refresh = new() { Name = "WorkspaceTreeRefresh", MinWidth = 0, MinHeight = 0,
        Padding = new Thickness(5, 3, 5, 3), Content = new FontIcon { Glyph = RefreshGlyph, FontSize = 12 } };
    private readonly StackPanel _rows = new() { Spacing = 1 };
    private readonly ScrollViewer _scroll = new() { Name = "WorkspaceTree", VerticalScrollBarVisibility = ScrollBarVisibility.Auto,
        HorizontalScrollMode = ScrollMode.Disabled, HorizontalScrollBarVisibility = ScrollBarVisibility.Disabled,
        Visibility = Visibility.Collapsed };
    private readonly TextBlock _status = new() { FontSize = 11, TextWrapping = TextWrapping.Wrap,
        Visibility = Visibility.Collapsed, Margin = new Thickness(2, 6, 2, 0) };
    private readonly TextBlock _empty = new() { FontSize = 12, TextWrapping = TextWrapping.Wrap,
        Margin = new Thickness(2, 12, 2, 0), Visibility = Visibility.Collapsed };
    private readonly DispatcherTimer _poll = new() { Interval = TimeSpan.FromSeconds(2) };
    private readonly HashSet<string> _expanded = new(StringComparer.Ordinal);
    private IReadOnlyList<WorkspaceTreeEntry> _entries = [];
    private IReadOnlyList<WorkspaceTreeAttachment> _attachments = [];
    private string? _attachmentFolder;
    private Guid? _conversation;
    private string? _version;
    private CancellationTokenSource? _loading;
    private bool _enabled, _refreshing, _disposed;

    /// <summary>Raised when the user returns from the mirror to the right-pane menu.
    /// 用户从镜像返回右栏菜单时触发。</summary>
    public event EventHandler? BackRequested;

    /// <summary>Raised when a mirrored file should open in the existing preview surface.
    /// 镜像中的文件需要打开时触发，复用既有预览面。</summary>
    public event EventHandler<string>? FileOpenRequested;

    /// <summary>Raised when a saved upload should open through the attachment endpoints.
    /// 已保存上传需要打开时触发，走附件接口。</summary>
    public event EventHandler<WorkspaceTreeAttachment>? AttachmentOpenRequested;

    public ConversationWorkspaceTree()
    {
        RowDefinitions.Add(new() { Height = GridLength.Auto });
        RowDefinitions.Add(new() { Height = new GridLength(1, GridUnitType.Star) });
        RowDefinitions.Add(new() { Height = GridLength.Auto });

        var header = new Grid { ColumnSpacing = 6, Margin = new Thickness(0, 0, 0, 6) };
        header.ColumnDefinitions.Add(new() { Width = GridLength.Auto });
        header.ColumnDefinitions.Add(new() { Width = GridLength.Auto });
        header.ColumnDefinitions.Add(new() { Width = new GridLength(1, GridUnitType.Star) });
        header.ColumnDefinitions.Add(new() { Width = GridLength.Auto });
        var icon = new FontIcon { Glyph = FolderGlyph, FontSize = 13, VerticalAlignment = VerticalAlignment.Center };
        var label = new StackPanel { Spacing = 1, VerticalAlignment = VerticalAlignment.Center };
        label.Children.Add(_title);
        label.Children.Add(_hint);
        _back.Click += (_, _) => BackRequested?.Invoke(this, EventArgs.Empty);
        Grid.SetColumn(label, 2);
        Grid.SetColumn(_refresh, 3);
        header.Children.Add(_back);
        Grid.SetColumn(icon, 1);
        header.Children.Add(icon);
        header.Children.Add(label);
        header.Children.Add(_refresh);

        var resources = Application.Current.Resources;
        if (resources.TryGetValue("KynxaCompactIconButtonStyle", out var compact) && compact is Style compactStyle)
        {
            _refresh.Style = compactStyle;
            _back.Style = compactStyle;
        }
        if (resources.TryGetValue("KynxaSecondaryTextBrush", out var secondary) && secondary is Brush brush)
            icon.Foreground = _hint.Foreground = _status.Foreground = _empty.Foreground = _refresh.Foreground = _back.Foreground = brush;
        AutomationProperties.SetName(_refresh, UiText.Get("刷新工作区文件"));
        ToolTipService.SetToolTip(_refresh, UiText.Get("刷新工作区文件"));
        AutomationProperties.SetName(_back, UiText.Get("返回工作区菜单"));
        ToolTipService.SetToolTip(_back, UiText.Get("返回工作区菜单"));

        _scroll.Content = _rows;
        _refresh.Click += (_, _) => _ = RefreshAsync();
        _poll.Tick += (_, _) => _ = RefreshAsync();
        Children.Add(header);
        Grid.SetRow(_scroll, 1);
        Children.Add(_scroll);
        var footer = new StackPanel { Spacing = 4 };
        footer.Children.Add(_status);
        footer.Children.Add(_empty);
        Grid.SetRow(footer, 2);
        Children.Add(footer);
        UiText.LanguageChanged += LanguageChanged;
        RefreshLanguage();
    }

    /// <summary>Switching conversation resets the version so the next read returns a full snapshot.
    /// 切换聊天会清空版本，使下一次读取返回完整快照。</summary>
    public void ShowConversation(Guid conversationId)
    {
        if (_disposed || _conversation == conversationId) return;
        _conversation = conversationId;
        _version = null;
        _expanded.Clear();
        _rows.Children.Clear();
        _entries = [];
        _attachments = [];
        _attachmentFolder = null;
        _status.Visibility = Visibility.Collapsed;
        _empty.Visibility = Visibility.Collapsed;
        if (_enabled) _ = RefreshAsync();
    }

    /// <summary>Only a visible pane keeps polling; hiding it cancels the pending read.
    /// 只有可见面板持续轮询；隐藏时取消未完成的读取。</summary>
    public void SetPreviewEnabled(bool enabled)
    {
        if (_disposed || _enabled == enabled) return;
        _enabled = enabled;
        if (enabled)
        {
            _poll.Start();
            _ = RefreshAsync();
        }
        else
        {
            _poll.Stop();
            _loading?.Cancel();
            _version = null;
        }
    }

    private async Task RefreshAsync()
    {
        if (_disposed || !_enabled || _refreshing || _conversation is not Guid conversation) return;
        _refreshing = true;
        _loading?.Cancel();
        var cancellation = _loading = new CancellationTokenSource();
        try
        {
            var snapshot = await _client.ReadAsync(conversation, _version, cancellation.Token);
            if (cancellation.IsCancellationRequested || _conversation != conversation) return;
            // An unchanged version keeps the current rows, so polling never redraws an idle tree.
            // 版本未变化时保留现有行，轮询不会重绘空闲的文件树。
            if (_version == snapshot.Version) return;
            _version = snapshot.Version;
            Render(snapshot);
        }
        catch (OperationCanceledException) { }
        catch (Exception) when (!cancellation.IsCancellationRequested)
        {
            ShowStatus(UiText.Get("工作区文件暂时无法读取。"));
        }
        finally { _refreshing = false; }
    }

    private void Render(WorkspaceTreeSnapshot snapshot)
    {
        _title.Text = snapshot.WorkspaceKind == "isolated" ? UiText.Get("本会话文件") : UiText.Get("已挂载文件夹");
        _hint.Text = snapshot.Root;
        ToolTipService.SetToolTip(_hint, snapshot.Root);
        _entries = snapshot.Entries;
        _attachments = snapshot.Attachments;
        _attachmentFolder = snapshot.VirtualAttachmentFolder;
        Rebuild();
        _empty.Text = UiText.Get("工作目录还没有文件。模型生成或执行产生的文件会出现在这里。");
        _empty.Visibility = _scroll.Visibility == Visibility.Visible ? Visibility.Collapsed : Visibility.Visible;
        _status.Text = snapshot.Truncated ? UiText.Get("文件较多，仅显示了一部分。") : "";
        _status.Visibility = snapshot.Truncated ? Visibility.Visible : Visibility.Collapsed;
    }

    /// <summary>Rows are rebuilt from the snapshot and the expanded set, so expansion survives version refreshes.
    /// 行按快照与展开集合重建，展开状态在版本刷新后保持。</summary>
    private void Rebuild()
    {
        _rows.Children.Clear();
        var children = new Dictionary<string, List<WorkspaceTreeEntry>>(StringComparer.Ordinal);
        foreach (var entry in _entries)
        {
            string parent = ParentPath(entry.Path);
            if (!children.TryGetValue(parent, out var items)) children[parent] = items = [];
            items.Add(entry);
        }
        AddLevel(children, "", 0);
        if (_attachments.Count > 0) AddAttachmentFolder();
        _scroll.Visibility = _rows.Children.Count > 0 ? Visibility.Visible : Visibility.Collapsed;
    }

    private void AddLevel(Dictionary<string, List<WorkspaceTreeEntry>> children, string parent, int depth)
    {
        if (!children.TryGetValue(parent, out var items)) return;
        foreach (var entry in items
            .OrderBy(entry => entry.Kind == "directory" ? 0 : 1)
            .ThenBy(entry => entry.Name, StringComparer.OrdinalIgnoreCase))
        {
            bool folder = entry.Kind == "directory";
            bool expanded = folder && _expanded.Contains(entry.Path);
            var row = new WorkspaceRow(folder ? (expanded ? ExpandedGlyph : CollapsedGlyph) : " ",
                folder ? FolderGlyph : FileGlyph, entry.Name, StatusText(entry.Status), depth);
            var selected = entry;
            row.Click += (_, _) =>
            {
                if (!folder)
                {
                    FileOpenRequested?.Invoke(this, selected.Path);
                    return;
                }
                if (!_expanded.Remove(selected.Path)) _expanded.Add(selected.Path);
                Rebuild();
            };
            _rows.Children.Add(row);
            if (expanded) AddLevel(children, entry.Path, depth + 1);
        }
    }

    private void AddAttachmentFolder()
    {
        string name = string.IsNullOrEmpty(_attachmentFolder) ? "_attachments" : _attachmentFolder;
        bool expanded = _expanded.Contains(name);
        var folder = new WorkspaceRow(expanded ? ExpandedGlyph : CollapsedGlyph, FolderGlyph, name, "", 0);
        folder.Click += (_, _) =>
        {
            if (!_expanded.Remove(name)) _expanded.Add(name);
            Rebuild();
        };
        _rows.Children.Add(folder);
        if (!expanded) return;
        foreach (var attachment in _attachments)
        {
            var saved = attachment;
            var row = new WorkspaceRow(" ", FileGlyph, saved.Name, "", 1);
            row.Click += (_, _) => AttachmentOpenRequested?.Invoke(this, saved);
            _rows.Children.Add(row);
        }
    }

    private static string StatusText(string? status) => status switch
    {
        "added" => UiText.Get("新增"),
        "modified" => UiText.Get("已修改"),
        _ => "",
    };

    private static string ParentPath(string path)
    {
        int separator = path.LastIndexOf('/');
        return separator <= 0 ? "" : path[..separator];
    }

    private void ShowStatus(string message)
    {
        _status.Text = message;
        _status.Visibility = Visibility.Visible;
    }

    private void LanguageChanged(object? sender, EventArgs args)
    {
        if (_disposed) return;
        if (DispatcherQueue.HasThreadAccess) RefreshLanguage();
        else DispatcherQueue.TryEnqueue(RefreshLanguage);
    }

    private void RefreshLanguage()
    {
        if (_disposed) return;
        AutomationProperties.SetName(_refresh, UiText.Get("刷新工作区文件"));
        ToolTipService.SetToolTip(_refresh, UiText.Get("刷新工作区文件"));
        AutomationProperties.SetName(_back, UiText.Get("返回工作区菜单"));
        ToolTipService.SetToolTip(_back, UiText.Get("返回工作区菜单"));
        AutomationProperties.SetName(_scroll, UiText.Get("工作区文件"));
        if (_entries.Count > 0) Rebuild();
    }

    public void Dispose()
    {
        if (_disposed) return;
        _disposed = true;
        _poll.Stop();
        _loading?.Cancel();
        _loading?.Dispose();
        _loading = null;
        _conversation = null;
        _version = null;
        _entries = [];
        _attachments = [];
        _expanded.Clear();
        _rows.Children.Clear();
        UiText.LanguageChanged -= LanguageChanged;
        _client.Dispose();
    }

    /// <summary>A row is a flat button with aligned chevron, glyph, name and status; no template indirection.
    /// 一行是扁平按钮，内含对齐的展开箭头、图标、名称与状态，不经过模板间接层。</summary>
    private sealed class WorkspaceRow : Button
    {
        public WorkspaceRow(string chevron, string glyph, string name, string status, int depth)
        {
            MinWidth = 0; MinHeight = 28; Padding = new Thickness(2 + depth * 14, 3, 6, 3);
            Background = new SolidColorBrush(Microsoft.UI.Colors.Transparent);
            BorderThickness = new Thickness(0);
            HorizontalAlignment = HorizontalAlignment.Stretch;
            HorizontalContentAlignment = HorizontalAlignment.Left;
            CornerRadius = new CornerRadius(6);
            AutomationProperties.SetName(this, name);
            ToolTipService.SetToolTip(this, name);
            var content = new Grid { ColumnSpacing = 6 };
            content.ColumnDefinitions.Add(new() { Width = new GridLength(12) });
            content.ColumnDefinitions.Add(new() { Width = new GridLength(14) });
            content.ColumnDefinitions.Add(new() { Width = new GridLength(1, GridUnitType.Star) });
            content.ColumnDefinitions.Add(new() { Width = GridLength.Auto });
            var chevronIcon = new FontIcon { Glyph = chevron, FontSize = 10, VerticalAlignment = VerticalAlignment.Center };
            var glyphIcon = new FontIcon { Glyph = glyph, FontSize = 12, VerticalAlignment = VerticalAlignment.Center };
            var label = new TextBlock { Text = name, FontSize = 12, TextTrimming = TextTrimming.CharacterEllipsis,
                VerticalAlignment = VerticalAlignment.Center };
            var statusLabel = new TextBlock { Text = status, FontSize = 10, Opacity = 0.7,
                VerticalAlignment = VerticalAlignment.Center, Margin = new Thickness(6, 0, 0, 0) };
            Grid.SetColumn(glyphIcon, 1);
            Grid.SetColumn(label, 2);
            Grid.SetColumn(statusLabel, 3);
            content.Children.Add(chevronIcon);
            content.Children.Add(glyphIcon);
            content.Children.Add(label);
            content.Children.Add(statusLabel);
            Content = content;
        }
    }
}

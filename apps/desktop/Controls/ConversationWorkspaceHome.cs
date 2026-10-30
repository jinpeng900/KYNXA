using KYNXA_Desktop.Services;
using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Automation;
using Microsoft.UI.Xaml.Controls;
using Microsoft.UI.Xaml.Input;
using Microsoft.UI.Xaml.Media;
using Windows.System;

namespace KYNXA_Desktop.Controls;

/// <summary>
/// Workspace entry points share the right pane; website entry never opens a modal dialog.
/// 工作区入口共用右栏，网址直接输入，不再弹出阻断式输入窗口。
/// </summary>
public sealed class ConversationWorkspaceHome : Grid
{
    private readonly TextBox _address = new() { PlaceholderText = "https://", FontSize = 13, MinHeight = 34,
        CornerRadius = new CornerRadius(9), BorderThickness = new Thickness(0),
        Padding = new Thickness(10, 5, 10, 5), MinWidth = 0, UseSystemFocusVisuals = false };
    private readonly TextBlock _directory = new() { FontSize = 12, TextWrapping = TextWrapping.Wrap,
        TextTrimming = TextTrimming.CharacterEllipsis, MaxLines = 2 };
    private readonly StackPanel _files = new() { Spacing = 2 };
    private readonly ScrollViewer _fileScroll;
    private readonly ScrollViewer _homeScroll;
    private readonly ScrollViewer _folderEditorHost = new() { Visibility = Visibility.Collapsed,
        HorizontalScrollBarVisibility = ScrollBarVisibility.Disabled, VerticalScrollBarVisibility = ScrollBarVisibility.Auto };
    private WorkspaceFolderEditor? _folderEditor;
    private readonly Grid _filePane = new() { Visibility = Visibility.Collapsed, RowSpacing = 10, Margin = new Thickness(14) };
    private readonly SegmentedSelector _fileKinds = new([
        new("files", "文件", "\uE8B7"), new("attachments", "已保存附件", "\uE723")]);
    private readonly SegmentedSelector _attachmentScopes = new([new("chat", "本聊天"), new("work", "本工作")]);
    private readonly StackPanel _pages = new() { Orientation = Orientation.Horizontal, Spacing = 8 };
    private readonly Dictionary<string, WorkspaceDirectoryListing> _directoryCache = new(StringComparer.OrdinalIgnoreCase);
    private readonly HashSet<string> _expandedDirectories = new(StringComparer.OrdinalIgnoreCase);
    private readonly SemaphoreSlim _directoryReads = new(3, 3);
    private CancellationTokenSource _browserReads = new();
    private long _browserGeneration;
    private bool _updatingBrowser, _hasProject;
    private SavedAttachmentPage? _attachmentPage;
    private readonly TextBlock _fileNotice = new() { FontSize = 12, TextWrapping = TextWrapping.Wrap };
    private readonly CompactInfoBar _fileError = new() { Severity = InfoBarSeverity.Error, IsIconVisible = false };
    private readonly TextBlock _fileLocation = new() { FontSize = 11, TextTrimming = TextTrimming.CharacterEllipsis,
        MaxLines = 1, Margin = new Thickness(2, 0, 2, 0) };
    private WorkspaceDirectoryListing? _listing;
    private string? _listingRoot;
    public event EventHandler<string?>? DirectoryRequested;
    public event EventHandler<string>? WorkspaceFileRequested;
    public event EventHandler<string>? WebsiteRequested;
    public event EventHandler? ChangesRequested;
    public event EventHandler? TerminalRequested;
    public event EventHandler? FileRequested;
    public event EventHandler? LocalFileRequested;
    public event EventHandler? FileBrowserClosed;
    public event EventHandler? RefreshRequested;
    public event EventHandler<bool>? AttachmentsRequested;
    public event EventHandler<int>? AttachmentPageRequested;
    public event EventHandler<SavedAttachmentEntry>? SavedAttachmentRequested;
    public Func<string, CancellationToken, Task<WorkspaceDirectoryListing>>? DirectoryLoader { get; set; }
    public bool IsFileBrowserVisible => _filePane.Visibility == Visibility.Visible;
    public bool IsFolderEditorVisible => _folderEditor is not null;
    public bool IsShowingAttachments => IsFileBrowserVisible && _fileKinds.SelectedKey == "attachments";
    public bool UsesProjectAttachments => _hasProject && _attachmentScopes.SelectedKey == "work";
    private readonly Button _changes, _terminal, _file, _navigate, _localFile, _browserLocalFile;
    private readonly Dictionary<TextBlock, string> _labels = [];
    private string? _directoryPath;
    private bool _hasConversation, _languageSubscribed;

    public ConversationWorkspaceHome()
    {
        var content = new StackPanel { Spacing = 16, MaxWidth = 460, HorizontalAlignment = HorizontalAlignment.Center,
            VerticalAlignment = VerticalAlignment.Center, Margin = new Thickness(18) };
        var homeLayout = new Grid();
        homeLayout.Children.Add(content);
        _homeScroll = new ScrollViewer { Content = homeLayout, VerticalContentAlignment = VerticalAlignment.Stretch,
            HorizontalContentAlignment = HorizontalAlignment.Stretch, HorizontalScrollBarVisibility = ScrollBarVisibility.Disabled,
            VerticalScrollBarVisibility = ScrollBarVisibility.Auto };
        _homeScroll.SizeChanged += (_, args) =>
        {
            if (args.NewSize.Width <= 0 || args.NewSize.Height <= 0) return;
            // Center within the viewport; a shorter window still scrolls the whole entry group.
            // 按可见区域居中；矮窗口仍能滚到整组入口的底部。
            homeLayout.MinHeight = args.NewSize.Height;
            content.Width = Math.Min(460, Math.Max(0, args.NewSize.Width - 36));
        };
        content.Children.Add(CreateHomeHeading());
        content.Children.Add(CreateLocationCard());
        _navigate = CreateButton("\uE72A", "打开网页");
        _navigate.Content = new FontIcon { Glyph = "\uE72A", FontSize = 14 };
        _navigate.Width = 32; _navigate.Height = 32; _navigate.MinHeight = 0;
        _navigate.Padding = new Thickness(6); _navigate.Margin = new Thickness(0, 2, 2, 2);
        _navigate.Background = new SolidColorBrush(Microsoft.UI.Colors.Transparent);
        _navigate.UseSystemFocusVisuals = false;
        _navigate.Click += (_, _) => Navigate();
        _address.KeyDown += (_, args) => { if (args.Key == VirtualKey.Enter) { args.Handled = true; Navigate(); } };
        content.Children.Add(CreateAddressEntry());
        var actions = new Grid { ColumnSpacing = 8 };
        for (int index = 0; index < 3; index++) actions.ColumnDefinitions.Add(new() { Width = new GridLength(1, GridUnitType.Star) });
        _changes = CreateButton("\uE8A9", "变更", centerContent: true);
        _terminal = CreateButton("\uE756", "终端", centerContent: true);
        _file = CreateButton("\uE8A5", "文件", centerContent: true);
        _changes.Click += (_, _) => ChangesRequested?.Invoke(this, EventArgs.Empty);
        _terminal.Click += (_, _) => TerminalRequested?.Invoke(this, EventArgs.Empty);
        _file.Click += (_, _) => FileRequested?.Invoke(this, EventArgs.Empty);
        actions.Children.Add(_changes); Grid.SetColumn(_terminal, 1); actions.Children.Add(_terminal);
        Grid.SetColumn(_file, 2); actions.Children.Add(_file); content.Children.Add(actions);
        _localFile = CreateButton("\uE8A5", "选择本机文件", centerContent: true);
        _localFile.Click += (_, _) => LocalFileRequested?.Invoke(this, EventArgs.Empty);
        content.Children.Add(_localFile);
        _browserLocalFile = CreateButton("\uE8A5", "选择本机文件", centerContent: true);
        _browserLocalFile.Padding = new Thickness(8, 6, 8, 6);
        _browserLocalFile.Click += (_, _) => LocalFileRequested?.Invoke(this, EventArgs.Empty);
        _fileNotice.Foreground = Brush("KynxaSecondaryTextBrush");
        _files.Children.Add(_fileNotice);
        _fileScroll = new ScrollViewer { Content = _files,
            HorizontalScrollBarVisibility = ScrollBarVisibility.Disabled, VerticalScrollBarVisibility = ScrollBarVisibility.Auto };
        Children.Add(_homeScroll);
        CreateFilePane();
        Children.Add(_folderEditorHost);
        _directory.Foreground = Brush("KynxaSecondaryTextBrush");
        _address.Foreground = Brush("KynxaTextBrush");
        AutomationProperties.SetName(_address, UiText.Get("网页地址"));
        Loaded += (_, _) =>
        {
            if (_languageSubscribed) return;
            _languageSubscribed = true; UiText.LanguageChanged += LanguageChanged; RefreshLanguage();
        };
        Unloaded += (_, _) =>
        {
            _languageSubscribed = false; UiText.LanguageChanged -= LanguageChanged;
            _browserReads.Cancel();
        };
    }

    private Border CreateAddressEntry()
    {
        var transparent = new SolidColorBrush(Microsoft.UI.Colors.Transparent);
        var row = new Grid();
        row.ColumnDefinitions.Add(new() { Width = new GridLength(1, GridUnitType.Star) });
        row.ColumnDefinitions.Add(new() { Width = GridLength.Auto });
        row.Children.Add(_address); Grid.SetColumn(_navigate, 1); row.Children.Add(_navigate);
        var frame = new Border { Child = row, CornerRadius = new CornerRadius(10),
            BorderThickness = new Thickness(1), BorderBrush = Brush("KynxaDividerBrush"),
            Background = Brush("KynxaReplySurfaceBrush") };
        _address.Background = _address.BorderBrush = transparent;
        foreach (string key in new[] { "TextControlBackground", "TextControlBackgroundPointerOver", "TextControlBackgroundFocused",
            "TextControlBorderBrush", "TextControlBorderBrushPointerOver", "TextControlBorderBrushFocused" }) _address.Resources[key] = transparent;
        _address.Resources["TextControlBorderThemeThickness"] = new Thickness(0);
        _address.Resources["TextControlBorderThemeThicknessFocused"] = new Thickness(0);
        void UpdateFocus() => frame.BorderBrush = Brush(_address.FocusState != FocusState.Unfocused || _navigate.FocusState != FocusState.Unfocused
            ? "KynxaSecondaryTextBrush" : "KynxaDividerBrush");
        _address.GotFocus += (_, _) => UpdateFocus(); _address.LostFocus += (_, _) => UpdateFocus();
        _navigate.GotFocus += (_, _) => UpdateFocus(); _navigate.LostFocus += (_, _) => UpdateFocus();
        return frame;
    }

    private Grid CreateHomeHeading()
    {
        var heading = new Grid { ColumnSpacing = 12 };
        heading.ColumnDefinitions.Add(new() { Width = GridLength.Auto });
        heading.ColumnDefinitions.Add(new() { Width = new GridLength(1, GridUnitType.Star) });
        heading.Children.Add(new Border { Width = 44, Height = 44, CornerRadius = new CornerRadius(14),
            Background = Brush("KynxaSelectionBrush"), Child = new FontIcon { Glyph = "\uE80F", FontSize = 21,
                Foreground = Brush("KynxaAccentTextBrush") } });
        var text = new StackPanel { Spacing = 3, VerticalAlignment = VerticalAlignment.Center };
        foreach (var (key, size) in new[] { ("工作区", 20d), ("查看文件、网页与工作成果", 12d) })
        {
            var label = new TextBlock { Text = UiText.Get(key), FontSize = size, TextWrapping = TextWrapping.Wrap,
                Foreground = Brush(size == 20 ? "KynxaTextBrush" : "KynxaSecondaryTextBrush") };
            _labels[label] = key; text.Children.Add(label);
        }
        Grid.SetColumn(text, 1); heading.Children.Add(text);
        return heading;
    }

    private Border CreateLocationCard()
    {
        var location = new StackPanel { Spacing = 6 };
        var title = new TextBlock { Text = UiText.Get("工作文件夹"), FontSize = 11,
            Foreground = Brush("KynxaSecondaryTextBrush") };
        _labels[title] = "工作文件夹";
        location.Children.Add(title); location.Children.Add(_directory);
        return new Border { Padding = new Thickness(12), CornerRadius = new CornerRadius(12),
            BorderBrush = Brush("KynxaDividerBrush"), BorderThickness = new Thickness(1),
            Background = Brush("KynxaReplySurfaceBrush"), Child = location };
    }

    public void ShowDirectory(string? directory, bool hasConversation)
    {
        _directoryPath = directory; _hasConversation = hasConversation;
        _directory.Text = string.IsNullOrEmpty(directory) ? UiText.Get("未关联工作文件夹") : directory;
        ToolTipService.SetToolTip(_directory, _directory.Text);
        _changes.IsEnabled = hasConversation && !string.IsNullOrEmpty(directory);
        _terminal.IsEnabled = hasConversation;
        _file.IsEnabled = hasConversation;
        _localFile.IsEnabled = hasConversation;
        _browserLocalFile.IsEnabled = hasConversation;
        _address.IsEnabled = hasConversation;
        _navigate.IsEnabled = hasConversation;
    }

    public void ShowFolderEditor(WorkspaceFolderEditor editor)
    {
        CancelFolderEditor();
        HideFiles();
        _folderEditor = editor;
        _folderEditorHost.Content = editor;
        _folderEditorHost.Visibility = Visibility.Visible;
        _homeScroll.Visibility = Visibility.Collapsed;
        editor.Closed += (_, _) =>
        {
            if (!ReferenceEquals(_folderEditor, editor)) return;
            _folderEditor = null;
            _folderEditorHost.Content = null; _folderEditorHost.Visibility = Visibility.Collapsed;
            _homeScroll.Visibility = Visibility.Visible;
        };
    }

    public void CancelFolderEditor() => _folderEditor?.Cancel();
    public void ShowFolderEditorError(string message) => _folderEditor?.ShowError(message);

    private void CreateFilePane()
    {
        for (int index = 0; index < 6; index++) _filePane.RowDefinitions.Add(new()
            { Height = index == 4 ? new GridLength(1, GridUnitType.Star) : GridLength.Auto });
        var toolbar = new Grid { ColumnSpacing = 8 };
        toolbar.ColumnDefinitions.Add(new() { Width = GridLength.Auto });
        toolbar.ColumnDefinitions.Add(new() { Width = new GridLength(1, GridUnitType.Star) });
        toolbar.ColumnDefinitions.Add(new() { Width = GridLength.Auto });
        var back = CreateButton("\uE72B", "返回工作区");
        back.Content = new FontIcon { Glyph = "\uE72B", FontSize = 14 };
        back.Click += (_, _) => { HideFiles(); FileBrowserClosed?.Invoke(this, EventArgs.Empty); };
        toolbar.Children.Add(back);
        var title = new TextBlock { Text = UiText.Get("工作区文件"), FontSize = 15,
            TextTrimming = TextTrimming.CharacterEllipsis, VerticalAlignment = VerticalAlignment.Center,
            Foreground = Brush("KynxaTextBrush") };
        _labels[title] = "工作区文件";
        Grid.SetColumn(title, 1); toolbar.Children.Add(title);
        var refresh = CreateButton("\uE72C", "刷新列表");
        refresh.Content = new FontIcon { Glyph = "\uE72C", FontSize = 14 };
        refresh.Click += (_, _) => RefreshRequested?.Invoke(this, EventArgs.Empty);
        toolbar.Children.Add(refresh); Grid.SetColumn(refresh, 2);
        _filePane.Children.Add(toolbar);
        _fileLocation.Foreground = Brush("KynxaSecondaryTextBrush");
        _filePane.Children.Add(_fileLocation); Grid.SetRow(_fileLocation, 1);
        var sources = new Grid { ColumnSpacing = 8, RowSpacing = 8 };
        sources.ColumnDefinitions.Add(new() { Width = new GridLength(1, GridUnitType.Star) });
        sources.ColumnDefinitions.Add(new() { Width = GridLength.Auto });
        sources.RowDefinitions.Add(new() { Height = GridLength.Auto });
        sources.RowDefinitions.Add(new() { Height = GridLength.Auto });
        sources.Children.Add(_fileKinds); sources.Children.Add(_browserLocalFile);
        Grid.SetRow(_browserLocalFile, 1); Grid.SetColumnSpan(_browserLocalFile, 2);
        sources.SizeChanged += (_, args) =>
        {
            bool inline = args.NewSize.Width >= 480;
            sources.RowSpacing = inline ? 0 : 8;
            Grid.SetColumnSpan(_fileKinds, inline ? 1 : 2);
            Grid.SetRow(_browserLocalFile, inline ? 0 : 1);
            Grid.SetColumn(_browserLocalFile, inline ? 1 : 0);
            Grid.SetColumnSpan(_browserLocalFile, inline ? 1 : 2);
        };
        _filePane.Children.Add(sources); Grid.SetRow(sources, 2);
        _filePane.Children.Add(_attachmentScopes); Grid.SetRow(_attachmentScopes, 3);
        _filePane.Children.Add(_fileScroll); Grid.SetRow(_fileScroll, 4);
        _filePane.Children.Add(_pages); Grid.SetRow(_pages, 5);
        AutomationProperties.SetAutomationId(_filePane, "WorkspaceFileBrowser");
        AutomationProperties.SetAutomationId(back, "WorkspaceFileBrowserBack");
        AutomationProperties.SetAutomationId(refresh, "WorkspaceFileBrowserRefresh");
        AutomationProperties.SetAutomationId(_browserLocalFile, "WorkspaceFileBrowserLocalFile");
        _fileKinds.SelectionChanged += (_, key) =>
        {
            if (_updatingBrowser) return;
            if (key == "files") FileRequested?.Invoke(this, EventArgs.Empty);
            else AttachmentsRequested?.Invoke(this, UsesProjectAttachments);
        };
        _attachmentScopes.SelectionChanged += (_, _) =>
        {
            if (!_updatingBrowser && IsShowingAttachments) AttachmentsRequested?.Invoke(this, UsesProjectAttachments);
        };
        Children.Add(_filePane);
    }

    public void ShowProjectScope(bool hasProject)
    {
        _hasProject = hasProject;
        _attachmentScopes.Visibility = IsShowingAttachments && hasProject ? Visibility.Visible : Visibility.Collapsed;
    }

    private void EnterFilePane(string kind)
    {
        _updatingBrowser = true;
        try { _fileKinds.SelectedKey = kind; }
        finally { _updatingBrowser = false; }
        _homeScroll.Visibility = Visibility.Collapsed; _filePane.Visibility = Visibility.Visible;
        _attachmentScopes.Visibility = kind == "attachments" && _hasProject ? Visibility.Visible : Visibility.Collapsed;
        _pages.Children.Clear();
        _fileLocation.Text = kind == "attachments" ? UiText.Get("已保存附件") : _listing?.Path ?? _directoryPath ?? UiText.Get("未关联工作文件夹");
        ToolTipService.SetToolTip(_fileLocation, _fileLocation.Text);
    }

    private void ResetDirectoryReads()
    {
        ++_browserGeneration;
        _browserReads.Cancel(); _browserReads.Dispose(); _browserReads = new();
        _directoryCache.Clear();
    }

    public void ShowFilesLoading()
    {
        EnterFilePane("files"); ResetDirectoryReads(); _attachmentPage = null;
        _listing = null;
        _files.Children.Clear(); _fileNotice.Text = UiText.Get("正在读取文件列表…");
        _files.Children.Add(_fileNotice); _fileScroll.Visibility = Visibility.Visible;
    }

    public void HideFiles()
    {
        ResetDirectoryReads(); _expandedDirectories.Clear(); DirectoryLoader = null;
        _fileError.IsOpen = false;
        _listing = null; _attachmentPage = null; _files.Children.Clear();
        _filePane.Visibility = Visibility.Collapsed;
        _homeScroll.Visibility = IsFolderEditorVisible ? Visibility.Collapsed : Visibility.Visible;
    }

    public void ShowFilesError(string message)
    {
        _files.Children.Clear(); _fileError.Message = message; _fileError.IsOpen = true;
        _files.Children.Add(_fileError);
    }

    public void ShowFiles(WorkspaceDirectoryListing listing, string? rootPath)
    {
        EnterFilePane("files");
        _listing = listing; _listingRoot = rootPath;
        _fileLocation.Text = listing.Path; ToolTipService.SetToolTip(_fileLocation, listing.Path);
        _files.Children.Clear(); _fileScroll.Visibility = Visibility.Visible;
        if (rootPath is not null && !string.Equals(Path.TrimEndingDirectorySeparator(listing.Path),
            Path.TrimEndingDirectorySeparator(rootPath), StringComparison.OrdinalIgnoreCase))
        {
            var parent = CreateButton("\uE70E", UiText.Get("上级文件夹"), translate: false);
            parent.Click += (_, _) => DirectoryRequested?.Invoke(this, Path.GetDirectoryName(listing.Path));
            _files.Children.Add(parent);
        }
        RenderDirectoryEntries(listing, _files);
        if (listing.Entries.Length == 0 || listing.Truncated)
        {
            _fileNotice.Text = UiText.Get(listing.Truncated ? "文件较多，仅显示当前目录的部分条目。" : "此文件夹为空。");
            _files.Children.Add(_fileNotice);
        }
    }

    private void RenderDirectoryEntries(WorkspaceDirectoryListing listing, StackPanel container)
    {
        foreach (var entry in listing.Entries.OrderBy(entry => entry.Type == "directory" ? 0 : 1)
            .ThenBy(entry => entry.Name, StringComparer.OrdinalIgnoreCase))
        {
            string path = Path.Combine(listing.Path, entry.Name);
            var label = new Grid { ColumnSpacing = 8, MinWidth = 0 };
            label.ColumnDefinitions.Add(new() { Width = GridLength.Auto });
            label.ColumnDefinitions.Add(new() { Width = new GridLength(1, GridUnitType.Star) });
            label.Children.Add(new FontIcon { Glyph = entry.Type == "directory" ? "\uE8B7" : "\uE8A5", FontSize = 13 });
            var name = new TextBlock { Text = entry.Name, FontSize = 12, TextTrimming = TextTrimming.CharacterEllipsis,
                Foreground = Brush("KynxaTextBrush"), VerticalAlignment = VerticalAlignment.Center };
            Grid.SetColumn(name, 1); label.Children.Add(name);
            if (entry.Type == "directory")
            {
                var children = new StackPanel { Spacing = 2, Margin = new Thickness(8, 0, 0, 0) };
                var folder = new Expander { Header = label, Content = children, CornerRadius = new CornerRadius(8),
                    HorizontalAlignment = HorizontalAlignment.Stretch, HorizontalContentAlignment = HorizontalAlignment.Stretch,
                    MinWidth = 0, Padding = new Thickness(2), Background = Brush("KynxaReplySurfaceBrush"),
                    FocusVisualPrimaryBrush = Brush("KynxaSecondaryTextBrush"),
                    FocusVisualSecondaryBrush = new SolidColorBrush(Microsoft.UI.Colors.Transparent) };
                AutomationProperties.SetAutomationId(folder, "WorkspaceFolder_" + entry.Name);
                ToolTipService.SetToolTip(folder, path);
                folder.Expanding += async (_, _) =>
                {
                    _expandedDirectories.Add(path);
                    if (children.Children.Count == 0) await ExpandDirectoryAsync(path, children);
                };
                folder.Collapsed += (_, _) => _expandedDirectories.Remove(path);
                container.Children.Add(folder);
                if (_expandedDirectories.Contains(path)) folder.IsExpanded = true;
            }
            else
            {
                var button = CreateButton("\uE8A5", entry.Name, translate: false);
                button.Content = label; button.HorizontalContentAlignment = HorizontalAlignment.Stretch;
                button.Padding = new Thickness(8, 7, 8, 7);
                ToolTipService.SetToolTip(button, path);
                AutomationProperties.SetAutomationId(button, "WorkspaceFile_" + entry.Name);
                button.Click += (_, _) => WorkspaceFileRequested?.Invoke(this, path);
                container.Children.Add(button);
            }
        }
    }

    private async Task ExpandDirectoryAsync(string path, StackPanel children)
    {
        if (DirectoryLoader is not { } loader) return;
        long generation = _browserGeneration;
        CancellationToken token = _browserReads.Token;
        // Mark admission immediately so collapsing/reopening a pending branch cannot dispatch it twice.
        // 立即占位，避免目录尚在排队或读取时快速收起、再展开导致重复派发。
        children.Children.Add(new TextBlock { Text = UiText.Get("正在读取文件列表…"), FontSize = 12,
            Foreground = Brush("KynxaSecondaryTextBrush") });
        bool admitted = false;
        try
        {
            if (!_directoryCache.TryGetValue(path, out var listing))
            {
                await _directoryReads.WaitAsync(token); admitted = true;
                listing = await loader(path, token);
                if (generation != _browserGeneration || token.IsCancellationRequested) return;
                if (_directoryCache.Count >= 128) _directoryCache.Remove(_directoryCache.Keys.First());
                _directoryCache[path] = listing;
            }
            if (generation != _browserGeneration || token.IsCancellationRequested) return;
            children.Children.Clear(); RenderDirectoryEntries(listing, children);
            if (listing.Entries.Length == 0) children.Children.Add(new TextBlock { Text = UiText.Get("此文件夹为空。"), FontSize = 12 });
        }
        catch (OperationCanceledException) { }
        catch (Exception error) when (error is HttpRequestException or InvalidDataException or InvalidOperationException or System.Text.Json.JsonException)
        {
            if (generation != _browserGeneration) return;
            children.Children.Clear();
            var retry = CreateButton("\uE72C", "文件夹读取失败，可重试。");
            retry.Click += async (_, _) => { children.Children.Clear(); await ExpandDirectoryAsync(path, children); };
            children.Children.Add(retry);
        }
        finally { if (admitted) _directoryReads.Release(); }
    }

    public void ShowAttachmentsLoading()
    {
        EnterFilePane("attachments"); ResetDirectoryReads(); _listing = null; _attachmentPage = null;
        _files.Children.Clear(); _fileNotice.Text = UiText.Get("正在读取附件…"); _files.Children.Add(_fileNotice);
    }

    public void ShowAttachments(SavedAttachmentPage page)
    {
        EnterFilePane("attachments"); _attachmentPage = page; _listing = null; _files.Children.Clear();
        foreach (var entry in page.Attachments)
        {
            var button = CreateButton("\uE723", entry.Name, translate: false);
            button.HorizontalContentAlignment = HorizontalAlignment.Stretch;
            var text = new StackPanel { Spacing = 3 };
            text.Children.Add(new TextBlock { Text = entry.Name, FontSize = 12, TextTrimming = TextTrimming.CharacterEllipsis });
            text.Children.Add(new TextBlock { Text = $"{entry.ConversationTitle} · {entry.SizeBytes:N0} B", FontSize = 11,
                Foreground = Brush("KynxaSecondaryTextBrush"), TextTrimming = TextTrimming.CharacterEllipsis });
            button.Content = text;
            ToolTipService.SetToolTip(button, $"{entry.Name}\n{entry.ConversationTitle} · {entry.SizeBytes:N0} B");
            AutomationProperties.SetAutomationId(button, "SavedAttachment_" + entry.Id.ToString("D"));
            button.Click += (_, _) => SavedAttachmentRequested?.Invoke(this, entry);
            _files.Children.Add(button);
        }
        if (page.Attachments.Length == 0)
        {
            _fileNotice.Text = UiText.Get("此范围内还没有已保存的附件。"); _files.Children.Add(_fileNotice);
        }
        if (page.Offset > 0)
        {
            var previous = CreateButton("\uE72B", "上一页");
            previous.Click += (_, _) => AttachmentPageRequested?.Invoke(this, Math.Max(0, page.Offset - page.Limit));
            _pages.Children.Add(previous);
        }
        if (page.HasMore && page.NextOffset is int offset)
        {
            var next = CreateButton("\uE72A", "下一页");
            next.Click += (_, _) => AttachmentPageRequested?.Invoke(this, offset); _pages.Children.Add(next);
        }
    }

    public void FocusAddress() => _address.Focus(FocusState.Programmatic);

    private void Navigate()
    {
        if (WorkspaceNavigationAddress.TryResolve(_address.Text, out var address))
            WebsiteRequested?.Invoke(this, address.AbsoluteUri);
    }

    private Button CreateButton(string glyph, string text, bool translate = true, bool centerContent = false)
    {
        var label = new Grid { ColumnSpacing = 7, MinWidth = 0 };
        label.ColumnDefinitions.Add(new() { Width = GridLength.Auto });
        label.ColumnDefinitions.Add(new() { Width = centerContent ? GridLength.Auto : new GridLength(1, GridUnitType.Star) });
        label.Children.Add(new FontIcon { Glyph = glyph, FontSize = 13 });
        var caption = new TextBlock { Text = translate ? UiText.Get(text) : text, FontSize = 12,
            TextTrimming = TextTrimming.CharacterEllipsis, VerticalAlignment = VerticalAlignment.Center };
        Grid.SetColumn(caption, 1); label.Children.Add(caption);
        var button = new Button { Content = label, CornerRadius = new CornerRadius(9), Padding = new Thickness(8, 11, 8, 11),
            MinWidth = 0, HorizontalAlignment = HorizontalAlignment.Stretch,
            HorizontalContentAlignment = centerContent ? HorizontalAlignment.Center : HorizontalAlignment.Stretch,
            Background = Brush("KynxaReplySurfaceBrush"),
            BorderBrush = Brush("KynxaDividerBrush"), Foreground = Brush("KynxaTextBrush"),
            FocusVisualPrimaryBrush = Brush("KynxaSecondaryTextBrush"), FocusVisualSecondaryBrush = new SolidColorBrush(Microsoft.UI.Colors.Transparent), Tag = text };
        if (Application.Current.Resources.TryGetValue("KynxaQuietButtonStyle", out var style) && style is Style quiet) button.Style = quiet;
        AutomationProperties.SetName(button, translate ? UiText.Get(text) : text);
        ToolTipService.SetToolTip(button, translate ? UiText.Get(text) : text);
        return button;
    }

    private void LanguageChanged(object? sender, EventArgs args) => DispatcherQueue.TryEnqueue(RefreshLanguage);
    private void RefreshLanguage()
    {
        foreach (var (label, key) in _labels) label.Text = UiText.Get(key);
        foreach (var button in new[] { _changes, _terminal, _file, _navigate, _localFile, _browserLocalFile })
        {
            string caption = UiText.Get((string)button.Tag);
            if (button.Content is Panel panel && panel.Children.OfType<TextBlock>().FirstOrDefault() is { } label) label.Text = caption;
            AutomationProperties.SetName(button, caption); ToolTipService.SetToolTip(button, caption);
        }
        AutomationProperties.SetName(_address, UiText.Get("网页地址"));
        ShowDirectory(_directoryPath, _hasConversation);
        if (_listing is { } listing) ShowFiles(listing, _listingRoot);
        else if (_attachmentPage is { } page) ShowAttachments(page);
    }

    private static Brush Brush(string key) => (Brush)Application.Current.Resources[key];
}

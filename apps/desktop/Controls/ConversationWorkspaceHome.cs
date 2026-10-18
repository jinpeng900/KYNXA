using KYNXA_Desktop.Services;
using KYNXA_Desktop.Models.UI;
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
        CornerRadius = new CornerRadius(9), Padding = new Thickness(10, 5, 10, 5), MinWidth = 0 };
    private readonly TextBlock _directory = new() { FontSize = 12, TextWrapping = TextWrapping.Wrap,
        TextAlignment = TextAlignment.Center, MaxWidth = 420 };
    private readonly ComboBox _roots = new() { HorizontalAlignment = HorizontalAlignment.Stretch, MinWidth = 0,
        DisplayMemberPath = nameof(ProjectMountState.Name), Visibility = Visibility.Collapsed };
    private readonly StackPanel _files = new() { Spacing = 2 };
    private readonly ScrollViewer _fileScroll;
    private readonly TextBlock _fileNotice = new() { FontSize = 12, TextWrapping = TextWrapping.Wrap };
    private bool _updatingRoots;
    private Guid? _selectedMountId;
    private WorkspaceDirectoryListing? _listing;
    private string? _listingRoot;
    public event EventHandler<Guid>? MountRequested;
    public event EventHandler<string?>? DirectoryRequested;
    public event EventHandler<string>? WorkspaceFileRequested;
    public Guid? SelectedMountId => _selectedMountId;
    public event EventHandler<string>? WebsiteRequested;
    public event EventHandler? ChangesRequested;
    public event EventHandler? TerminalRequested;
    public event EventHandler? FileRequested;
    private readonly Button _changes, _terminal, _file;
    private readonly Dictionary<TextBlock, string> _labels = [];
    private string? _directoryPath;
    private bool _hasConversation, _languageSubscribed;

    public ConversationWorkspaceHome()
    {
        var content = new StackPanel { Spacing = 18, MaxWidth = 460, HorizontalAlignment = HorizontalAlignment.Stretch,
            VerticalAlignment = VerticalAlignment.Center, Margin = new Thickness(18) };
        var title = new TextBlock { Text = UiText.Get("工作区"), FontSize = 20,
            HorizontalAlignment = HorizontalAlignment.Center, Foreground = Brush("KynxaTextBrush") };
        _labels[title] = "工作区";
        content.Children.Add(title);
        content.Children.Add(_directory);
        content.Children.Add(_roots);
        _roots.FocusVisualPrimaryBrush = Brush("KynxaSecondaryTextBrush");
        _roots.FocusVisualSecondaryBrush = new SolidColorBrush(Microsoft.UI.Colors.Transparent);
        _roots.CornerRadius = new CornerRadius(8);
        _roots.Background = Brush("KynxaReplySurfaceBrush");
        _roots.Foreground = Brush("KynxaTextBrush");
        _roots.BorderBrush = Brush("KynxaDividerBrush");
        _roots.Resources["ComboBoxBorderBrushFocused"] = Brush("KynxaSecondaryTextBrush");
        _roots.Resources["ComboBoxSelectionIndicatorBrush"] = Brush("KynxaSecondaryTextBrush");
        AutomationProperties.SetAutomationId(_roots, "WorkspaceMountSelector");
        AutomationProperties.SetName(_roots, UiText.Get("工作文件夹"));
        _roots.SelectionChanged += (_, _) =>
        {
            if (_updatingRoots || _roots.SelectedItem is not ProjectMountState mount || _selectedMountId == mount.Id) return;
            _selectedMountId = mount.Id; MountRequested?.Invoke(this, mount.Id);
        };
        var addressRow = new Grid { ColumnSpacing = 5 };
        addressRow.ColumnDefinitions.Add(new() { Width = new GridLength(1, GridUnitType.Star) });
        addressRow.ColumnDefinitions.Add(new() { Width = GridLength.Auto });
        addressRow.Children.Add(_address);
        var navigate = CreateButton("\uE72A", "打开网页");
        navigate.Padding = new Thickness(9); Grid.SetColumn(navigate, 1); addressRow.Children.Add(navigate);
        navigate.Click += (_, _) => Navigate();
        _address.KeyDown += (_, args) => { if (args.Key == VirtualKey.Enter) { args.Handled = true; Navigate(); } };
        content.Children.Add(addressRow);
        var actions = new Grid { ColumnSpacing = 8 };
        for (int index = 0; index < 3; index++) actions.ColumnDefinitions.Add(new() { Width = new GridLength(1, GridUnitType.Star) });
        _changes = CreateButton("\uE8A9", "变更");
        _terminal = CreateButton("\uE756", "终端");
        _file = CreateButton("\uE8A5", "文件");
        _changes.Click += (_, _) => ChangesRequested?.Invoke(this, EventArgs.Empty);
        _terminal.Click += (_, _) => TerminalRequested?.Invoke(this, EventArgs.Empty);
        _file.Click += (_, _) => FileRequested?.Invoke(this, EventArgs.Empty);
        actions.Children.Add(_changes); Grid.SetColumn(_terminal, 1); actions.Children.Add(_terminal);
        Grid.SetColumn(_file, 2); actions.Children.Add(_file); content.Children.Add(actions);
        _fileNotice.Foreground = Brush("KynxaSecondaryTextBrush");
        _files.Children.Add(_fileNotice);
        _fileScroll = new ScrollViewer { Content = _files, MaxHeight = 300, Visibility = Visibility.Collapsed,
            HorizontalScrollBarVisibility = ScrollBarVisibility.Disabled, VerticalScrollBarVisibility = ScrollBarVisibility.Auto };
        content.Children.Add(_fileScroll);
        Children.Add(content);
        _directory.Foreground = Brush("KynxaSecondaryTextBrush");
        _address.Foreground = Brush("KynxaTextBrush");
        _address.Background = Brush("KynxaReplySurfaceBrush");
        _address.BorderBrush = Brush("KynxaDividerBrush");
        _address.FocusVisualPrimaryBrush = Brush("KynxaSecondaryTextBrush");
        _address.FocusVisualSecondaryBrush = new SolidColorBrush(Microsoft.UI.Colors.Transparent);
        _address.Resources["TextControlBorderBrushFocused"] = Brush("KynxaSecondaryTextBrush");
        _address.Resources["TextControlBorderBrushPointerOver"] = Brush("KynxaSecondaryTextBrush");
        AutomationProperties.SetName(_address, UiText.Get("网页地址"));
        Loaded += (_, _) =>
        {
            if (_languageSubscribed) return;
            _languageSubscribed = true; UiText.LanguageChanged += LanguageChanged; RefreshLanguage();
        };
        Unloaded += (_, _) =>
        {
            _languageSubscribed = false; UiText.LanguageChanged -= LanguageChanged;
        };
    }

    public void ShowDirectory(string? directory, bool hasConversation)
    {
        _directoryPath = directory; _hasConversation = hasConversation;
        _directory.Text = string.IsNullOrEmpty(directory) ? UiText.Get("未关联工作文件夹") : directory;
        _changes.IsEnabled = hasConversation && !string.IsNullOrEmpty(directory);
        _terminal.IsEnabled = hasConversation;
        _file.IsEnabled = hasConversation;
        _address.IsEnabled = hasConversation;
    }

    public void ShowMounts(IReadOnlyList<ProjectMountState> mounts, Guid? selectedMountId)
    {
        _updatingRoots = true;
        try
        {
            bool changed = _roots.ItemsSource is not IReadOnlyList<ProjectMountState> previous || !previous.SequenceEqual(mounts);
            if (changed) _roots.ItemsSource = mounts.ToArray();
            _selectedMountId = selectedMountId;
            _roots.SelectedItem = ((IEnumerable<ProjectMountState>?)_roots.ItemsSource)?.FirstOrDefault(mount => mount.Id == selectedMountId);
            _roots.Visibility = mounts.Count > 0 ? Visibility.Visible : Visibility.Collapsed;
        }
        finally { _updatingRoots = false; }
    }

    public void ShowFilesLoading()
    {
        _listing = null;
        _files.Children.Clear(); _fileNotice.Text = UiText.Get("正在读取文件列表…");
        _files.Children.Add(_fileNotice); _fileScroll.Visibility = Visibility.Visible;
    }

    public void HideFiles() { _listing = null; _files.Children.Clear(); _fileScroll.Visibility = Visibility.Collapsed; }

    public void ShowFilesError(string message)
    { _files.Children.Clear(); _fileNotice.Text = message; _files.Children.Add(_fileNotice); }

    public void ShowFiles(WorkspaceDirectoryListing listing, string? rootPath)
    {
        _listing = listing; _listingRoot = rootPath;
        _files.Children.Clear(); _fileScroll.Visibility = Visibility.Visible;
        if (rootPath is not null && !string.Equals(Path.TrimEndingDirectorySeparator(listing.Path),
            Path.TrimEndingDirectorySeparator(rootPath), StringComparison.OrdinalIgnoreCase))
        {
            var parent = CreateButton("\uE70E", UiText.Get("上级文件夹"), translate: false);
            parent.Click += (_, _) => DirectoryRequested?.Invoke(this, Path.GetDirectoryName(listing.Path));
            _files.Children.Add(parent);
        }
        foreach (var entry in listing.Entries.OrderBy(entry => entry.Type == "directory" ? 0 : 1).ThenBy(entry => entry.Name, StringComparer.OrdinalIgnoreCase))
        {
            bool directory = entry.Type == "directory";
            var button = CreateButton(directory ? "\uE8B7" : "\uE8A5", entry.Name, translate: false);
            button.Padding = new Thickness(8, 6, 8, 6);
            AutomationProperties.SetAutomationId(button, "WorkspaceFile_" + entry.Name);
            ToolTipService.SetToolTip(button, entry.Name);
            button.Click += (_, _) =>
            {
                string path = Path.Combine(listing.Path, entry.Name);
                if (directory) DirectoryRequested?.Invoke(this, path); else WorkspaceFileRequested?.Invoke(this, path);
            };
            _files.Children.Add(button);
        }
        if (listing.Entries.Length == 0 || listing.Truncated)
        {
            _fileNotice.Text = UiText.Get(listing.Truncated ? "文件较多，仅显示当前目录的部分条目。" : "此文件夹为空。");
            _files.Children.Add(_fileNotice);
        }
    }

    public void FocusAddress() => _address.Focus(FocusState.Programmatic);

    private void Navigate()
    {
        if (WorkspaceNavigationAddress.TryResolve(_address.Text, out var address))
            WebsiteRequested?.Invoke(this, address.AbsoluteUri);
    }

    private Button CreateButton(string glyph, string text, bool translate = true)
    {
        var label = new StackPanel { Orientation = Orientation.Horizontal, Spacing = 7, HorizontalAlignment = HorizontalAlignment.Center };
        label.Children.Add(new FontIcon { Glyph = glyph, FontSize = 13 });
        var caption = new TextBlock { Text = translate ? UiText.Get(text) : text, FontSize = 12 };
        if (translate) _labels[caption] = text;
        label.Children.Add(caption);
        var button = new Button { Content = label, CornerRadius = new CornerRadius(9), Padding = new Thickness(8, 11, 8, 11),
            MinWidth = 0, HorizontalAlignment = HorizontalAlignment.Stretch, Background = Brush("KynxaReplySurfaceBrush"),
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
        foreach (var button in new[] { _changes, _terminal, _file })
        {
            string caption = UiText.Get((string)button.Tag);
            AutomationProperties.SetName(button, caption); ToolTipService.SetToolTip(button, caption);
        }
        AutomationProperties.SetName(_address, UiText.Get("网页地址"));
        ShowDirectory(_directoryPath, _hasConversation);
        if (_listing is { } listing) ShowFiles(listing, _listingRoot);
    }

    private static Brush Brush(string key) => (Brush)Application.Current.Resources[key];
}

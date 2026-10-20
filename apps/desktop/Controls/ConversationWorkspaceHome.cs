using KYNXA_Desktop.Services;
using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Automation;
using Microsoft.UI.Xaml.Controls;
using Microsoft.UI.Xaml.Input;
using Microsoft.UI.Xaml.Media;
using Windows.System;

namespace KYNXA_Desktop.Controls;

/// <summary>
/// Right-pane menu: work files first, then terminal and changes, with the website entry kept at the bottom.
/// 右栏菜单：工作区文件在最上，其后依次为终端与变更；网址入口保持在最下方。
/// </summary>
public sealed class ConversationWorkspaceHome : Grid
{
    private readonly TextBox _address = new() { PlaceholderText = "https://", FontSize = 13, MinHeight = 34,
        CornerRadius = new CornerRadius(9), Padding = new Thickness(10, 5, 10, 5), MinWidth = 0 };
    private readonly TextBlock _directory = new() { FontSize = 11, TextWrapping = TextWrapping.Wrap, MaxLines = 2,
        TextTrimming = TextTrimming.CharacterEllipsis, HorizontalAlignment = HorizontalAlignment.Left };
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
        RowDefinitions.Add(new() { Height = new GridLength(1, GridUnitType.Star) });
        RowDefinitions.Add(new() { Height = GridLength.Auto });

        // The menu sits in the middle of the free area, lifted slightly above true centre so it stays clear of the
        // address footer below it.
        // 菜单位于空白区域中部，比正中略高一些，避免贴近下方网址页脚。
        var menu = new StackPanel { Spacing = 6, Margin = new Thickness(14, 0, 14, 24),
            VerticalAlignment = VerticalAlignment.Center };
        _file = CreateMenuButton("\uE8B7", "工作区文件");
        _terminal = CreateMenuButton("\uE756", "终端");
        _changes = CreateMenuButton("\uE8A9", "变更");
        _file.Click += (_, _) => FileRequested?.Invoke(this, EventArgs.Empty);
        _terminal.Click += (_, _) => TerminalRequested?.Invoke(this, EventArgs.Empty);
        _changes.Click += (_, _) => ChangesRequested?.Invoke(this, EventArgs.Empty);
        menu.Children.Add(_file);
        menu.Children.Add(_terminal);
        menu.Children.Add(_changes);
        Children.Add(menu);

        var footer = new StackPanel { Spacing = 9, Margin = new Thickness(14, 0, 14, 14) };
        footer.Children.Add(_directory);
        var addressRow = new Grid { ColumnSpacing = 5 };
        addressRow.ColumnDefinitions.Add(new() { Width = new GridLength(1, GridUnitType.Star) });
        addressRow.ColumnDefinitions.Add(new() { Width = GridLength.Auto });
        addressRow.Children.Add(_address);
        var navigate = CreateButton("\uE72A", "打开网页");
        navigate.Padding = new Thickness(9); Grid.SetColumn(navigate, 1); addressRow.Children.Add(navigate);
        navigate.Click += (_, _) => Navigate();
        _address.KeyDown += (_, args) => { if (args.Key == VirtualKey.Enter) { args.Handled = true; Navigate(); } };
        footer.Children.Add(addressRow);
        Grid.SetRow(footer, 1);
        Children.Add(footer);

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
        _terminal.IsEnabled = hasConversation && !string.IsNullOrEmpty(directory);
        _file.IsEnabled = hasConversation;
        _address.IsEnabled = hasConversation;
    }

    public void FocusAddress() => _address.Focus(FocusState.Programmatic);

    private void Navigate()
    {
        if (WorkspaceNavigationAddress.TryResolve(_address.Text, out var address))
            WebsiteRequested?.Invoke(this, address.AbsoluteUri);
    }

    /// <summary>A menu row keeps the existing button look but reads left-aligned like the reference layout.
    /// 菜单行保留现有按钮外观，但像参考布局那样左对齐排布。</summary>
    private Button CreateMenuButton(string glyph, string text)
    {
        var label = new StackPanel { Orientation = Orientation.Horizontal, Spacing = 10,
            HorizontalAlignment = HorizontalAlignment.Left, VerticalAlignment = VerticalAlignment.Center };
        label.Children.Add(new FontIcon { Glyph = glyph, FontSize = 15 });
        var caption = new TextBlock { Text = UiText.Get(text), FontSize = 13 };
        _labels[caption] = text; label.Children.Add(caption);
        var button = new Button { Content = label, CornerRadius = new CornerRadius(9), Padding = new Thickness(12, 10, 12, 10),
            MinWidth = 0, MinHeight = 0, HorizontalAlignment = HorizontalAlignment.Stretch,
            HorizontalContentAlignment = HorizontalAlignment.Left,
            Background = Brush("KynxaReplySurfaceBrush"), BorderBrush = Brush("KynxaDividerBrush"),
            Foreground = Brush("KynxaTextBrush"), FocusVisualPrimaryBrush = Brush("KynxaSecondaryTextBrush"),
            FocusVisualSecondaryBrush = new SolidColorBrush(Microsoft.UI.Colors.Transparent), Tag = text };
        if (Application.Current.Resources.TryGetValue("KynxaQuietButtonStyle", out var style) && style is Style quiet) button.Style = quiet;
        AutomationProperties.SetName(button, UiText.Get(text));
        ToolTipService.SetToolTip(button, UiText.Get(text));
        return button;
    }

    private Button CreateButton(string glyph, string text)
    {
        var label = new StackPanel { Orientation = Orientation.Horizontal, Spacing = 7, HorizontalAlignment = HorizontalAlignment.Center };
        label.Children.Add(new FontIcon { Glyph = glyph, FontSize = 13 });
        var caption = new TextBlock { Text = UiText.Get(text), FontSize = 12 };
        _labels[caption] = text; label.Children.Add(caption);
        var button = new Button { Content = label, CornerRadius = new CornerRadius(9), Padding = new Thickness(8, 11, 8, 11),
            MinWidth = 0, HorizontalAlignment = HorizontalAlignment.Stretch, Background = Brush("KynxaReplySurfaceBrush"),
            BorderBrush = Brush("KynxaDividerBrush"), Foreground = Brush("KynxaTextBrush"),
            FocusVisualPrimaryBrush = Brush("KynxaSecondaryTextBrush"), FocusVisualSecondaryBrush = new SolidColorBrush(Microsoft.UI.Colors.Transparent), Tag = text };
        if (Application.Current.Resources.TryGetValue("KynxaQuietButtonStyle", out var style) && style is Style quiet) button.Style = quiet;
        AutomationProperties.SetName(button, UiText.Get(text));
        ToolTipService.SetToolTip(button, UiText.Get(text));
        return button;
    }

    private void LanguageChanged(object? sender, EventArgs args) => DispatcherQueue.TryEnqueue(RefreshLanguage);
    private void RefreshLanguage()
    {
        foreach (var (label, key) in _labels) label.Text = UiText.Get(key);
        foreach (var button in new[] { _file, _terminal, _changes })
        {
            string caption = UiText.Get((string)button.Tag);
            AutomationProperties.SetName(button, caption); ToolTipService.SetToolTip(button, caption);
        }
        AutomationProperties.SetName(_address, UiText.Get("网页地址"));
        ShowDirectory(_directoryPath, _hasConversation);
    }

    private static Brush Brush(string key) => (Brush)Application.Current.Resources[key];
}

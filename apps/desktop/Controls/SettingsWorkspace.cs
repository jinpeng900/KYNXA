using KYNXA_Desktop.Services;
using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Automation;
using Microsoft.UI.Xaml.Controls;
using Microsoft.UI.Xaml.Input;
using Microsoft.UI.Xaml.Media;
using Windows.System;

namespace KYNXA_Desktop.Controls;

/// <summary>
/// Keeps settings categories in one window and retains each lazily created page.
/// 在同一窗口中展示设置分类，按需创建并保留各页的编辑和滚动状态。
/// </summary>
public sealed partial class SettingsWorkspace : UserControl, IDisposable
{
    private sealed record Category(string Key, string LabelKey, Button Button, Func<FrameworkElement> Create, FrameworkElement? HeaderAction);
    private readonly List<Category> _categories = [];
    private readonly Dictionary<string, FrameworkElement> _pages = new(StringComparer.Ordinal);
    private readonly StackPanel _navigation = new() { Spacing = 5, Padding = new Thickness(12, 16, 12, 16) };
    private readonly Grid _pageHost = new();
    private readonly TextBlock _heading;
    private readonly ContentControl _headerAction = new() { HorizontalContentAlignment = HorizontalAlignment.Right,
        VerticalContentAlignment = VerticalAlignment.Center, Visibility = Visibility.Collapsed };
    private readonly Border _sidebar;
    private readonly Grid _layout;
    private bool _disposed;

    public string? SelectedKey { get; private set; }
    public event EventHandler<string>? PageChanged;

    public SettingsWorkspace(FontFamily font)
    {
        Resources["ControlCornerRadius"] = new CornerRadius(8);
        foreach (string key in new[] { "TextControlBorderBrushFocused", "ComboBoxBorderBrushFocused",
            "ComboBoxBackgroundBorderBrushFocused", "SystemControlFocusVisualPrimaryBrush", "ListViewItemFocusBorderBrush" })
            Resources[key] = AppearanceService.GetBrush("KynxaSecondaryTextBrush");
        _layout = new Grid { ColumnSpacing = 0 };
        _layout.ColumnDefinitions.Add(new ColumnDefinition { Width = new GridLength(184) });
        _layout.ColumnDefinitions.Add(new ColumnDefinition { Width = new GridLength(1, GridUnitType.Star) });
        _sidebar = new Border { Child = new ScrollViewer { Content = _navigation,
            HorizontalScrollBarVisibility = ScrollBarVisibility.Disabled,
            VerticalScrollBarVisibility = ScrollBarVisibility.Auto }, BorderThickness = new Thickness(0, 0, 1, 0) };
        AutomationProperties.SetAutomationId(_navigation, "SettingsNavigation");
        _layout.Children.Add(_sidebar);
        var content = new Grid();
        content.RowDefinitions.Add(new RowDefinition { Height = GridLength.Auto });
        content.RowDefinitions.Add(new RowDefinition { Height = new GridLength(1, GridUnitType.Star) });
        var header = new Grid { Padding = new Thickness(24, 20, 24, 18), ColumnSpacing = 12,
            HorizontalAlignment = HorizontalAlignment.Left, MaxWidth = 820 };
        header.ColumnDefinitions.Add(new ColumnDefinition { Width = new GridLength(1, GridUnitType.Star) });
        header.ColumnDefinitions.Add(new ColumnDefinition { Width = GridLength.Auto });
        _heading = new TextBlock { FontFamily = font, FontSize = 24,
            FontWeight = Microsoft.UI.Text.FontWeights.SemiBold,
            TextWrapping = TextWrapping.Wrap };
        AutomationProperties.SetAutomationId(_heading, "SettingsPageHeading");
        header.Children.Add(_heading);
        Grid.SetColumn(_headerAction, 1);
        header.Children.Add(_headerAction);
        content.Children.Add(header);
        content.SizeChanged += (_, args) => header.Width = Math.Min(820, Math.Max(0, args.NewSize.Width));
        Grid.SetRow(_pageHost, 1);
        AutomationProperties.SetAutomationId(_pageHost, "SettingsPageHost");
        content.Children.Add(_pageHost);
        Grid.SetColumn(content, 1);
        _layout.Children.Add(content);
        Content = _layout;
        SizeChanged += (_, args) => _layout.ColumnDefinitions[0].Width = new GridLength(args.NewSize.Width < 760 ? 152 : 184);
        AppearanceService.Changed += RefreshAppearance;
        UiText.LanguageChanged += RefreshAppearance;
        RefreshAppearance(null, EventArgs.Empty);
    }

    public Button AddPage(string key, string labelKey, string iconGlyph, Func<FrameworkElement> create, FontFamily font,
        FrameworkElement? headerAction = null)
    {
        var body = new StackPanel { Orientation = Orientation.Horizontal, Spacing = 10 };
        body.Children.Add(new FontIcon { Glyph = iconGlyph, FontSize = 16,
            FontFamily = new FontFamily("Segoe Fluent Icons"), IsHitTestVisible = false });
        var label = new TextBlock { FontFamily = font, FontSize = 14, VerticalAlignment = VerticalAlignment.Center };
        UiLocalization.Bind(label, TextBlock.TextProperty, labelKey);
        body.Children.Add(label);
        var button = new Button { Content = body, HorizontalAlignment = HorizontalAlignment.Stretch,
            HorizontalContentAlignment = HorizontalAlignment.Left, Padding = new Thickness(12, 10, 12, 10),
            MinWidth = 0, CornerRadius = new CornerRadius(10), BorderThickness = new Thickness(0),
            Style = (Style)Application.Current.Resources["KynxaQuietButtonStyle"] };
        UiLocalization.Bind(button, AutomationProperties.NameProperty, labelKey);
        AutomationProperties.SetAutomationId(button, "SettingsCategory_" + key);
        button.Click += (_, _) => SelectPage(key);
        button.KeyDown += (_, args) => NavigateKey(key, args);
        _categories.Add(new(key, labelKey, button, create, headerAction));
        _navigation.Children.Add(button);
        RefreshAppearance(null, EventArgs.Empty);
        return button;
    }

    public void SelectPage(string key)
    {
        if (_disposed) return;
        var category = _categories.FirstOrDefault(item => item.Key == key);
        if (category is null) return;
        if (!_pages.TryGetValue(key, out var page))
        {
            page = category.Create();
            AutomationProperties.SetAutomationId(page, "SettingsPage_" + key);
            page.Visibility = Visibility.Collapsed;
            _pages.Add(key, page);
            _pageHost.Children.Add(page);
        }
        // Collapse rather than detach: drafts, list selections and scroll viewers keep their state.
        // 切换仅折叠旧页，不从可视树卸载，以保留草稿、列表选择和滚动位置。
        foreach (var entry in _pages) entry.Value.Visibility = entry.Key == key ? Visibility.Visible : Visibility.Collapsed;
        SelectedKey = key;
        _headerAction.Content = category.HeaderAction;
        _headerAction.Visibility = category.HeaderAction is null ? Visibility.Collapsed : Visibility.Visible;
        UiLocalization.Bind(_heading, TextBlock.TextProperty, category.LabelKey);
        RefreshAppearance(null, EventArgs.Empty);
        PageChanged?.Invoke(this, key);
    }

    public void ResetPage(string key)
    {
        if (!_pages.Remove(key, out var page)) return;
        _pageHost.Children.Remove(page);
        if (SelectedKey == key) SelectedKey = null;
    }

    public static ScrollViewer ScrollPage(FrameworkElement content)
    {
        var frame = new Border { Child = content, Padding = new Thickness(24, 0, 24, 24), MaxWidth = 820,
            HorizontalAlignment = HorizontalAlignment.Left, VerticalAlignment = VerticalAlignment.Top };
        var scroll = new ScrollViewer { Content = frame, HorizontalContentAlignment = HorizontalAlignment.Left,
            VerticalContentAlignment = VerticalAlignment.Top, ZoomMode = ZoomMode.Disabled,
            HorizontalScrollMode = ScrollMode.Disabled,
            HorizontalScrollBarVisibility = ScrollBarVisibility.Disabled,
            VerticalScrollBarVisibility = ScrollBarVisibility.Auto };
        void FitWidth(double viewportWidth)
        {
            if (!double.IsFinite(viewportWidth) || viewportWidth <= 0) return;
            double width = Math.Min(820, viewportWidth);
            if (double.IsNaN(frame.Width) || Math.Abs(frame.Width - width) > 0.1) frame.Width = width;
        }
        // A capped stretch child can be arranged wider than its measured extent; ScrollViewer then offsets it by the wrong width.
        // 仅限制 MaxWidth 的拉伸子项可能按小期望宽度计算偏移、却按大宽度排布；显式左对齐并按视口给出同一宽度。
        // Sizes are DIPs and change with the viewport; do not multiply content coordinates by monitor scale a second time.
        // 内容尺寸使用 DIP 并随视口变化，不再对坐标重复乘以屏幕缩放比例。
        scroll.SizeChanged += (_, args) => FitWidth(args.NewSize.Width);
        scroll.Loaded += (_, _) => FitWidth(scroll.ActualWidth);
        return scroll;
    }

    private void NavigateKey(string key, KeyRoutedEventArgs args)
    {
        if (args.Key is not (VirtualKey.Up or VirtualKey.Down or VirtualKey.Home or VirtualKey.End)) return;
        int index = _categories.FindIndex(item => item.Key == key);
        index = args.Key switch { VirtualKey.Home => 0, VirtualKey.End => _categories.Count - 1,
            VirtualKey.Up => (index - 1 + _categories.Count) % _categories.Count, _ => (index + 1) % _categories.Count };
        SelectPage(_categories[index].Key);
        _categories[index].Button.Focus(FocusState.Keyboard);
        args.Handled = true;
    }

    private void RefreshAppearance(object? sender, EventArgs args)
    {
        if (_disposed) return;
        _sidebar.Background = AppearanceService.GetBrush("KynxaSidebarBrush");
        _sidebar.BorderBrush = AppearanceService.GetBrush("KynxaDividerBrush");
        _heading.Foreground = AppearanceService.GetBrush("KynxaTextBrush");
        foreach (var category in _categories)
        {
            bool selected = category.Key == SelectedKey;
            category.Button.Background = selected ? AppearanceService.GetBrush("KynxaSelectionBrush")
                : new SolidColorBrush(Microsoft.UI.Colors.Transparent);
            category.Button.Foreground = AppearanceService.GetBrush("KynxaTextBrush");
            category.Button.FocusVisualPrimaryBrush = AppearanceService.GetBrush("KynxaSecondaryTextBrush");
            AutomationProperties.SetItemStatus(category.Button, UiText.Get(selected ? "已选中" : "未选中"));
        }
    }

    public void Dispose()
    {
        if (_disposed) return;
        _disposed = true;
        AppearanceService.Changed -= RefreshAppearance;
        UiText.LanguageChanged -= RefreshAppearance;
        _headerAction.Content = null;
        _pageHost.Children.Clear();
        _pages.Clear();
    }
}

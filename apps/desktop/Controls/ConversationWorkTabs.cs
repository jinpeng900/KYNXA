using KYNXA_Desktop.Services;
using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Automation;
using Microsoft.UI.Xaml.Controls;
using Microsoft.UI.Xaml.Media;
using Windows.ApplicationModel.DataTransfer;

namespace KYNXA_Desktop.Controls;

public sealed record ConversationWorkTabClosed(Guid ConversationId, ConversationWorkTab Tab);

/// <summary>
/// A neutral tab strip. The shell maps the selected logical resource to its existing content pane.
/// 通用标签栏；主界面把选中的逻辑资源映射到已有内容面板。
/// </summary>
public sealed class ConversationWorkTabs : Grid, IDisposable
{
    private const double TabTitleWidth = 190;
    private const double TabCloseWidth = 24;
    private readonly ConversationWorkTabState _state = new();
    private readonly StackPanel _tabs = new() { Orientation = Orientation.Horizontal, Spacing = 0 };
    private readonly Grid _tabViewport = new();
    private readonly ScrollViewer _scroll;
    private readonly Button _scrollLeft = ScrollButton("WorkTabsScrollLeft", "\uE76B");
    private readonly Button _scrollRight = ScrollButton("WorkTabsScrollRight", "\uE76C");
    private readonly Button _add = new() { Name = "WorkTabsAdd", Width = 28, Height = 26, MinWidth = 0, MinHeight = 0,
        Content = new FontIcon { Glyph = "\uE80F", FontSize = 12 }, Padding = new Thickness(0), BorderThickness = new Thickness(0) };
    private readonly Button _reopen = new() { Name = "WorkTabsReopen", Width = 26, Height = 26, MinWidth = 0, MinHeight = 0,
        Content = new FontIcon { Glyph = "\uE70D", FontSize = 10 }, Padding = new Thickness(0), BorderThickness = new Thickness(0) };
    private ConversationWorkTab[] _rendered = [];
    private string? _renderedSelected;
    private Guid? _renderedConversation;
    private double? _pendingHorizontalOffset;
    private bool _disposed, _scrollLayoutPending, _bringSelectedIntoView;

    public ConversationWorkTab? SelectedTab => _state.SelectedTab;
    public IReadOnlyList<ConversationWorkTab> Items => _state.Items;
    public IReadOnlyList<ConversationWorkTab> OpenItems => _state.OpenItems;
    public bool HasItems => _state.HasItems;
    public bool HasOpenTabs => _state.HasOpenTabs;
    public event EventHandler<ConversationWorkTab?>? SelectedChanged;
    public event EventHandler? TabsChanged;
    public event EventHandler? HomeRequested;
    public event EventHandler<ConversationWorkTab>? TabInvoked;
    public event EventHandler<ConversationWorkTabClosed>? TabClosed;

    public ConversationWorkTabs()
    {
        MinHeight = 26;
        ColumnDefinitions.Add(new() { Width = new GridLength(1, GridUnitType.Star) });
        ColumnDefinitions.Add(new() { Width = GridLength.Auto });
        ColumnDefinitions.Add(new() { Width = GridLength.Auto });
        var scroll = _scroll = new ScrollViewer { Name = "WorkTabsScroll", Content = _tabs, HorizontalScrollBarVisibility = ScrollBarVisibility.Hidden,
            VerticalScrollBarVisibility = ScrollBarVisibility.Disabled, HorizontalScrollMode = ScrollMode.Enabled, VerticalScrollMode = ScrollMode.Disabled,
            ZoomMode = ZoomMode.Disabled, Height = 26 };
        scroll.PointerWheelChanged += (_, args) =>
        {
            var point = args.GetCurrentPoint(scroll);
            if (!point.Properties.IsHorizontalMouseWheel && scroll.ScrollableWidth > 0)
            {
                scroll.ChangeView(Math.Clamp(scroll.HorizontalOffset - point.Properties.MouseWheelDelta / 2d, 0, scroll.ScrollableWidth), null, null, true);
                args.Handled = true;
            }
        };
        _tabViewport.ColumnDefinitions.Add(new() { Width = GridLength.Auto });
        _tabViewport.ColumnDefinitions.Add(new() { Width = new GridLength(1, GridUnitType.Star) });
        _tabViewport.ColumnDefinitions.Add(new() { Width = GridLength.Auto });
        _tabViewport.Children.Add(_scrollLeft);
        Grid.SetColumn(scroll, 1); _tabViewport.Children.Add(scroll);
        Grid.SetColumn(_scrollRight, 2); _tabViewport.Children.Add(_scrollRight);
        Children.Add(_tabViewport); Grid.SetColumn(_add, 1); Children.Add(_add);
        Grid.SetColumn(_reopen, 2); Children.Add(_reopen);
        ApplyQuietStyle(_add);
        ApplyQuietStyle(_reopen);
        foreach (var button in new[] { _scrollLeft, _scrollRight })
        {
            ApplyQuietStyle(button);
            button.FocusVisualSecondaryBrush = new SolidColorBrush(Microsoft.UI.Colors.Transparent);
            if (Application.Current.Resources.TryGetValue("KynxaSecondaryTextBrush", out var foreground) && foreground is Brush neutral)
                button.Foreground = button.FocusVisualPrimaryBrush = neutral;
        }
        if (Application.Current.Resources.TryGetValue("KynxaSecondaryTextBrush", out var brush) && brush is Brush secondary) _reopen.Foreground = secondary;
        _scrollLeft.Click += (_, _) => ScrollTabs(-1);
        _scrollRight.Click += (_, _) => ScrollTabs(1);
        _tabViewport.SizeChanged += (_, _) => QueueScrollLayout();
        _tabs.SizeChanged += (_, _) => QueueScrollLayout();
        scroll.ViewChanged += (_, _) => UpdateScrollButtons();
        _reopen.Click += (_, _) => ShowReopenMenu();
        _add.Click += (_, _) => { ShowHome(); HomeRequested?.Invoke(this, EventArgs.Empty); };
        UiText.LanguageChanged += LanguageChanged;
        RefreshLanguage(); Render();
    }

    public bool ShowConversation(Guid? conversationId, IReadOnlyList<ConversationWorkTab> items, bool autoOpenNew = true, bool catalogComplete = true)
    {
        if (_disposed) return false;
        var before = SelectedTab;
        Guid? beforeConversation = _state.ConversationId;
        bool added = _state.ShowConversation(conversationId, items, autoOpenNew, catalogComplete);
        bool changed = Render();
        Notify(before, beforeConversation, changed);
        return added;
    }

    public bool Select(string key) => Change(() => _state.Select(key));
    public bool Close(string key)
    {
        var tab = OpenItems.FirstOrDefault(item => item.Key == key);
        Guid? conversationId = _state.ConversationId;
        if (!Change(() => _state.Close(key))) return false;
        if (tab is not null && conversationId is Guid id) TabClosed?.Invoke(this, new(id, tab));
        return true;
    }
    public bool CloseSelected() => SelectedTab is { } selected && Close(selected.Key);
    public bool Reopen(string key) => Change(() => _state.Reopen(key));
    public bool Move(string key, string beforeKey) => Change(() => _state.Move(key, beforeKey));
    public bool ShowHome() => Change(_state.SelectHome);

    private bool Change(Func<bool> change)
    {
        if (_disposed) return false;
        var before = SelectedTab;
        bool accepted = change();
        if (accepted) Notify(before, _state.ConversationId, Render());
        return accepted;
    }

    private void Notify(ConversationWorkTab? before, Guid? beforeConversation, bool changed)
    {
        if (before?.Key != SelectedTab?.Key || beforeConversation != _state.ConversationId) SelectedChanged?.Invoke(this, SelectedTab);
        if (changed) TabsChanged?.Invoke(this, EventArgs.Empty);
    }

    private bool Render()
    {
        var open = OpenItems.ToArray();
        bool changed = _renderedConversation != _state.ConversationId || _renderedSelected != SelectedTab?.Key || !_rendered.SequenceEqual(open);
        _reopen.IsEnabled = HasItems;
        if (!changed) return false;
        bool sameConversation = _renderedConversation == _state.ConversationId;
        bool selectionChanged = !sameConversation || _renderedSelected != SelectedTab?.Key;
        double offset = sameConversation ? _scroll.HorizontalOffset : 0;
        _rendered = open; _renderedSelected = SelectedTab?.Key; _renderedConversation = _state.ConversationId;
        _tabs.Children.Clear();
        foreach (var item in open)
        {
            bool active = item.Key == SelectedTab?.Key;
            var row = new Grid { Name = "WorkTabItem", Tag = item.Key, Width = TabTitleWidth + 12 + TabCloseWidth, AllowDrop = true,
                CornerRadius = new CornerRadius(6), Margin = new Thickness(1, 0, 1, 0) };
            row.ColumnDefinitions.Add(new() { Width = new GridLength(TabTitleWidth + 12) });
            row.ColumnDefinitions.Add(new() { Width = new GridLength(TabCloseWidth) });
            var title = new TextBlock { Text = item.Title, FontSize = 12, Width = TabTitleWidth, TextTrimming = TextTrimming.CharacterEllipsis,
                VerticalAlignment = VerticalAlignment.Center };
            var select = new Button { Name = "WorkTabSelect", Tag = item.Key, Content = title, MinWidth = 0, MinHeight = 26,
                Padding = new Thickness(9, 2, 3, 2), BorderThickness = new Thickness(0), CornerRadius = new CornerRadius(6), CanDrag = true,
                HorizontalAlignment = HorizontalAlignment.Stretch, HorizontalContentAlignment = HorizontalAlignment.Stretch };
            var close = new Button { Name = "WorkTabClose", Tag = item.Key, Content = new FontIcon { Glyph = "\uE711", FontSize = 9 },
                Width = TabCloseWidth, Height = 26, MinWidth = 0, MinHeight = 0, Padding = new Thickness(0), BorderThickness = new Thickness(0), CornerRadius = new CornerRadius(6) };
            ApplyTabStyle(select); ApplyTabStyle(close);
            if (Application.Current.Resources.TryGetValue("KynxaSecondaryTextBrush", out var brush) && brush is Brush secondary)
            { title.Foreground = close.Foreground = secondary; }
            row.Background = active && Application.Current.Resources.TryGetValue("KynxaSelectionBrush", out var surface) && surface is Brush activeBackground
                ? activeBackground : new SolidColorBrush(Microsoft.UI.Colors.Transparent);
            var restingBackground = row.Background;
            row.PointerEntered += (_, _) =>
            {
                if (!active && Application.Current.Resources.TryGetValue("KynxaControlHoverBrush", out var hover) && hover is Brush background)
                    row.Background = background;
            };
            row.PointerExited += (_, _) => row.Background = restingBackground;
            select.Background = close.Background = new SolidColorBrush(Microsoft.UI.Colors.Transparent);
            AutomationProperties.SetName(select, item.Title); AutomationProperties.SetName(close, UiText.Get("关闭标签") + " · " + item.Title);
            AutomationProperties.SetHelpText(select, active ? UiText.Get("当前标签") : UiText.Get("切换标签"));
            ToolTipService.SetToolTip(select, item.Target ?? item.Title); ToolTipService.SetToolTip(close, UiText.Get("关闭标签"));
            select.Click += (_, _) => { Select(item.Key); TabInvoked?.Invoke(this, item); };
            close.Click += (_, _) => Close(item.Key);
            select.DragStarting += (_, args) =>
            {
                args.Data.SetText(item.Key); args.Data.RequestedOperation = DataPackageOperation.Move;
                args.Data.Properties["KynxaWorkTab"] = _state.ConversationId?.ToString("D") ?? "";
            };
            row.DragOver += (_, args) =>
            {
                if (args.DataView.Properties.TryGetValue("KynxaWorkTab", out var source) &&
                    source as string == _state.ConversationId?.ToString("D")) args.AcceptedOperation = DataPackageOperation.Move;
            };
            row.Drop += async (_, args) =>
            {
                Guid? conversation = _state.ConversationId;
                if (!args.DataView.Properties.TryGetValue("KynxaWorkTab", out var source) ||
                    source as string != conversation?.ToString("D")) return;
                string key = await args.DataView.GetTextAsync();
                if (!_disposed && _state.ConversationId == conversation) Move(key, item.Key);
            };
            row.Children.Add(select); Grid.SetColumn(close, 1); row.Children.Add(close); _tabs.Children.Add(row);
        }
        QueueScrollLayout(offset, selectionChanged);
        return true;
    }

    private static Button ScrollButton(string name, string glyph) => new()
    {
        Name = name, Content = new FontIcon { Glyph = glyph, FontSize = 10 }, Width = 24, Height = 26,
        MinWidth = 0, MinHeight = 0, Padding = new Thickness(0), BorderThickness = new Thickness(0),
        CornerRadius = new CornerRadius(6), Visibility = Visibility.Collapsed
    };

    private void ScrollTabs(int direction) => _scroll.ChangeView(
        Math.Clamp(_scroll.HorizontalOffset + direction * Math.Max(120, _scroll.ViewportWidth * 0.8), 0, _scroll.ScrollableWidth), null, null, true);

    private void QueueScrollLayout(double? offset = null, bool bringSelected = false)
    {
        if (_disposed) return;
        if (offset is not null) _pendingHorizontalOffset = offset;
        _bringSelectedIntoView |= bringSelected;
        if (_scrollLayoutPending) return;
        _scrollLayoutPending = true;
        _tabs.LayoutUpdated += ScrollLayoutUpdated;
    }

    private void ScrollLayoutUpdated(object? sender, object args)
    {
        if (_tabViewport.ActualWidth <= 0) return;
        // Measure the full tab viewport before showing overflow arrows.
        // 按完整标签视口判断溢出，避免箭头反复撑大布局；沿用正常布局流程。
        var visibility = _tabs.DesiredSize.Width > _tabViewport.ActualWidth + 0.5 ? Visibility.Visible : Visibility.Collapsed;
        if (_scrollLeft.Visibility != visibility)
        {
            _scrollLeft.Visibility = _scrollRight.Visibility = visibility;
            return;
        }
        _tabs.LayoutUpdated -= ScrollLayoutUpdated;
        _scrollLayoutPending = false;
        if (_pendingHorizontalOffset is double offset)
            _scroll.ChangeView(Math.Clamp(offset, 0, _scroll.ScrollableWidth), null, null, true);
        _pendingHorizontalOffset = null;
        bool bringSelected = _bringSelectedIntoView;
        _bringSelectedIntoView = false;
        if (bringSelected && SelectedTab is { } selected)
        {
            var row = _tabs.Children.OfType<FrameworkElement>().FirstOrDefault(row => row.Tag as string == selected.Key);
            row?.StartBringIntoView(new BringIntoViewOptions { AnimationDesired = false });
        }
        UpdateScrollButtons();
    }

    private void UpdateScrollButtons()
    {
        _scrollLeft.IsEnabled = _scroll.HorizontalOffset > 0.5;
        _scrollRight.IsEnabled = _scroll.HorizontalOffset < _scroll.ScrollableWidth - 0.5;
    }

    private void ShowReopenMenu()
    {
        if (_disposed || !HasItems) return;
        Guid? conversationId = _state.ConversationId;
        var menu = new MenuFlyout();
        foreach (var item in Items)
        {
            var entry = new MenuFlyoutItem { Text = item.Title, Tag = item.Key, FontSize = 12 };
            entry.Click += (_, _) =>
            {
                if (!_disposed && _state.ConversationId == conversationId)
                { Reopen(item.Key); TabInvoked?.Invoke(this, item); }
            };
            menu.Items.Add(entry);
        }
        menu.ShowAt(_reopen);
    }

    private static void ApplyQuietStyle(Button button)
    {
        if (Application.Current.Resources.TryGetValue("KynxaQuietButtonStyle", out var style) && style is Style quiet) button.Style = quiet;
    }

    private static void ApplyTabStyle(Button button)
    {
        if (Application.Current.Resources.TryGetValue("KynxaWorkspaceTabButtonStyle", out var style) && style is Style tabStyle)
            button.Style = tabStyle;
    }

    private void LanguageChanged(object? sender, EventArgs args)
    {
        if (_disposed) return;
        DispatcherQueue.TryEnqueue(() =>
        {
            if (_disposed) return;
            RefreshLanguage(); _rendered = []; Render(); TabsChanged?.Invoke(this, EventArgs.Empty);
        });
    }

    private void RefreshLanguage()
    {
        AutomationProperties.SetName(_reopen, UiText.Get("打开标签")); ToolTipService.SetToolTip(_reopen, UiText.Get("打开标签"));
        AutomationProperties.SetName(_add, UiText.Get("工作区主页")); ToolTipService.SetToolTip(_add, UiText.Get("工作区主页"));
        AutomationProperties.SetName(_scrollLeft, UiText.Get("向左滚动标签")); ToolTipService.SetToolTip(_scrollLeft, UiText.Get("向左滚动标签"));
        AutomationProperties.SetName(_scrollRight, UiText.Get("向右滚动标签")); ToolTipService.SetToolTip(_scrollRight, UiText.Get("向右滚动标签"));
    }

    public void Dispose()
    {
        if (_disposed) return;
        _disposed = true; UiText.LanguageChanged -= LanguageChanged; _tabs.LayoutUpdated -= ScrollLayoutUpdated;
        _state.Clear(); _tabs.Children.Clear();
    }
}

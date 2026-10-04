using KYNXA_Desktop.Services;
using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Automation;
using Microsoft.UI.Xaml.Controls;
using Microsoft.UI.Xaml.Media;

namespace KYNXA_Desktop.Controls;

/// <summary>
/// A neutral tab strip. The shell maps the selected logical resource to its existing content pane.
/// 通用标签栏；主界面把选中的逻辑资源映射到已有内容面板。
/// </summary>
public sealed class ConversationWorkTabs : Grid, IDisposable
{
    private readonly ConversationWorkTabState _state = new();
    private readonly StackPanel _tabs = new() { Orientation = Orientation.Horizontal, Spacing = 0 };
    private readonly ScrollViewer _scroll;
    private readonly Button _reopen = new() { Name = "WorkTabsReopen", Width = 26, Height = 28, MinWidth = 0, MinHeight = 0,
        Content = new FontIcon { Glyph = "\uE70D", FontSize = 10 }, Padding = new Thickness(0), BorderThickness = new Thickness(0) };
    private ConversationWorkTab[] _rendered = [];
    private string? _renderedSelected;
    private Guid? _renderedConversation;
    private bool _disposed;

    public ConversationWorkTab? SelectedTab => _state.SelectedTab;
    public IReadOnlyList<ConversationWorkTab> Items => _state.Items;
    public IReadOnlyList<ConversationWorkTab> OpenItems => _state.OpenItems;
    public bool HasItems => _state.HasItems;
    public bool HasOpenTabs => _state.HasOpenTabs;
    public event EventHandler<ConversationWorkTab?>? SelectedChanged;
    public event EventHandler? TabsChanged;

    public ConversationWorkTabs()
    {
        MinHeight = 32;
        ColumnDefinitions.Add(new() { Width = new GridLength(1, GridUnitType.Star) });
        ColumnDefinitions.Add(new() { Width = GridLength.Auto });
        var scroll = _scroll = new ScrollViewer { Name = "WorkTabsScroll", Content = _tabs, HorizontalScrollBarVisibility = ScrollBarVisibility.Auto,
            VerticalScrollBarVisibility = ScrollBarVisibility.Disabled, HorizontalScrollMode = ScrollMode.Enabled, VerticalScrollMode = ScrollMode.Disabled,
            ZoomMode = ZoomMode.Disabled, MaxHeight = 40 };
        scroll.PointerWheelChanged += (_, args) =>
        {
            var point = args.GetCurrentPoint(scroll);
            if (!point.Properties.IsHorizontalMouseWheel && scroll.ScrollableWidth > 0)
            {
                scroll.ChangeView(Math.Clamp(scroll.HorizontalOffset - point.Properties.MouseWheelDelta / 2d, 0, scroll.ScrollableWidth), null, null, true);
                args.Handled = true;
            }
        };
        Children.Add(scroll); Grid.SetColumn(_reopen, 1); Children.Add(_reopen);
        ApplyQuietStyle(_reopen);
        if (Application.Current.Resources.TryGetValue("KynxaSecondaryTextBrush", out var brush) && brush is Brush secondary) _reopen.Foreground = secondary;
        _reopen.Click += (_, _) => ShowReopenMenu();
        UiText.LanguageChanged += LanguageChanged;
        RefreshLanguage(); Render();
    }

    public bool ShowConversation(Guid? conversationId, IReadOnlyList<ConversationWorkTab> items, bool autoOpenNew = true)
    {
        if (_disposed) return false;
        var before = SelectedTab;
        Guid? beforeConversation = _state.ConversationId;
        bool added = _state.ShowConversation(conversationId, items, autoOpenNew);
        bool changed = Render();
        Notify(before, beforeConversation, changed);
        return added;
    }

    public bool Select(string key) => Change(() => _state.Select(key));
    public bool Close(string key) => Change(() => _state.Close(key));
    public bool CloseSelected() => Change(_state.CloseSelected);
    public bool Reopen(string key) => Change(() => _state.Reopen(key));

    private bool Change(Func<bool> change)
    {
        if (_disposed) return false;
        var before = SelectedTab;
        bool accepted = change();
        if (accepted)
        {
            Notify(before, _state.ConversationId, Render());
            if (before?.Key != SelectedTab?.Key && SelectedTab is { } selected)
                _tabs.Children.OfType<FrameworkElement>().FirstOrDefault(row => row.Tag as string == selected.Key)?.StartBringIntoView();
        }
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
        double offset = sameConversation ? _scroll.HorizontalOffset : 0;
        _rendered = open; _renderedSelected = SelectedTab?.Key; _renderedConversation = _state.ConversationId;
        _tabs.Children.Clear();
        foreach (var item in open)
        {
            bool active = item.Key == SelectedTab?.Key;
            var row = new Grid { Name = "WorkTabItem", Tag = item.Key, MinWidth = 70 };
            row.ColumnDefinitions.Add(new() { Width = GridLength.Auto }); row.ColumnDefinitions.Add(new() { Width = GridLength.Auto });
            var title = new TextBlock { Text = item.Title, FontSize = 12, MaxWidth = 130, TextTrimming = TextTrimming.CharacterEllipsis,
                VerticalAlignment = VerticalAlignment.Center };
            var select = new Button { Name = "WorkTabSelect", Tag = item.Key, Content = title, MinWidth = 0, MinHeight = 28,
                Padding = new Thickness(9, 4, 3, 4), BorderThickness = new Thickness(0), CornerRadius = new CornerRadius(0) };
            var close = new Button { Name = "WorkTabClose", Tag = item.Key, Content = new FontIcon { Glyph = "\uE711", FontSize = 9 },
                Width = 22, Height = 28, MinWidth = 0, MinHeight = 0, Padding = new Thickness(0), BorderThickness = new Thickness(0), CornerRadius = new CornerRadius(0) };
            ApplyQuietStyle(select); ApplyQuietStyle(close);
            if (Application.Current.Resources.TryGetValue("KynxaSecondaryTextBrush", out var brush) && brush is Brush secondary)
            { title.Foreground = close.Foreground = secondary; }
            row.Background = active && Application.Current.Resources.TryGetValue("KynxaReplySurfaceBrush", out var surface) && surface is Brush activeBackground
                ? activeBackground : new SolidColorBrush(Microsoft.UI.Colors.Transparent);
            select.Background = close.Background = new SolidColorBrush(Microsoft.UI.Colors.Transparent);
            AutomationProperties.SetName(select, item.Title); AutomationProperties.SetName(close, UiText.Get("关闭标签") + " · " + item.Title);
            AutomationProperties.SetHelpText(select, active ? UiText.Get("当前标签") : UiText.Get("切换标签"));
            ToolTipService.SetToolTip(select, item.Title); ToolTipService.SetToolTip(close, UiText.Get("关闭标签"));
            select.Click += (_, _) => Select(item.Key); close.Click += (_, _) => Close(item.Key);
            row.Children.Add(select); Grid.SetColumn(close, 1); row.Children.Add(close); _tabs.Children.Add(row);
        }
        _scroll.UpdateLayout();
        _scroll.ChangeView(offset, null, null, true);
        return true;
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
                if (!_disposed && _state.ConversationId == conversationId) Reopen(item.Key);
            };
            menu.Items.Add(entry);
        }
        menu.ShowAt(_reopen);
    }

    private static void ApplyQuietStyle(Button button)
    {
        if (Application.Current.Resources.TryGetValue("KynxaQuietButtonStyle", out var style) && style is Style quiet) button.Style = quiet;
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
    }

    public void Dispose()
    {
        if (_disposed) return;
        _disposed = true; UiText.LanguageChanged -= LanguageChanged; _state.Clear(); _tabs.Children.Clear();
    }
}

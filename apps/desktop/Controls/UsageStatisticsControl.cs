using Kynxa.UsageMonitor.Core;
using KYNXA_Desktop.Services;
using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Automation;
using Microsoft.UI.Xaml.Controls;
using Microsoft.UI.Xaml.Controls.Primitives;
using Microsoft.UI.Xaml.Media;
using FontWeights = Microsoft.UI.Text.FontWeights;

namespace KYNXA_Desktop.Controls;

/// <summary>
/// Shows the shared usage projection in the composer; storage and desktop-card ownership stay with the host.
/// 在输入框展示共享用量投影；存储及桌面悬浮卡生命周期仍由宿主负责。
/// </summary>
public sealed class UsageStatisticsControl : UserControl, IDisposable
{
    private readonly Button _button = new();
    private readonly StackPanel _inline = new() { Orientation = Orientation.Horizontal, Spacing = 6, VerticalAlignment = VerticalAlignment.Center };
    private readonly TextBlock _scope = Label();
    private readonly TextBlock _unit = Label();
    private readonly TextBlock _dot = Label("·");
    private readonly TextBlock _cacheLabel = Label();
    private readonly RollingNumberText _total = new();
    private readonly RollingNumberText _cache = new();
    private readonly FontIcon _chevron = new() { Glyph = "\uE70E", FontSize = 9, VerticalAlignment = VerticalAlignment.Center };
    private readonly Flyout _flyout = new() { Placement = FlyoutPlacementMode.TopEdgeAlignedRight };
    private readonly StackPanel _details = new() { Spacing = 9 };
    private readonly TextBlock _detailScope = Label();
    private readonly TextBlock _detailInput = Label();
    private readonly TextBlock _detailOutput = Label();
    private readonly TextBlock _detailHit = Label();
    private readonly TextBlock _description = Label();
    private readonly Button _floating = new();
    private readonly List<TextBlock> _secondaryLabels = [];
    private UsageDisplayState _state = new()
    {
        TotalText = "—",
        CacheText = "—",
        IsIncomplete = true,
        Description = UiText.Get("正在连接用量统计…")
    };
    private bool _isCompact;
    private bool _subscribed;
    private bool _disposed;

    public event EventHandler? FloatingRequested;

    public bool IsCompact
    {
        get => _isCompact;
        set
        {
            if (_isCompact == value) return;
            _isCompact = value;
            RefreshLabels();
        }
    }

    public UsageStatisticsControl()
    {
        HorizontalAlignment = HorizontalAlignment.Right;
        VerticalAlignment = VerticalAlignment.Center;
        foreach (var item in new UIElement[] { _scope, _total, _unit, _dot, _cacheLabel, _cache, _chevron })
            _inline.Children.Add(item);
        _button.Content = _inline;
        _button.MinHeight = 30;
        _button.Padding = new Thickness(7, 3, 7, 3);
        _button.CornerRadius = new CornerRadius(7);
        _button.HorizontalContentAlignment = HorizontalAlignment.Right;
        _button.Style = (Style)Application.Current.Resources["KynxaQuietButtonStyle"];
        _button.Click += ToggleDetails;
        AutomationProperties.SetAutomationId(_button, "UsageStatisticsButton");
        UiLocalization.SetAutomationHelp(_button, "点击展开用量明细");
        Content = _button;

        var presenter = new Style(typeof(FlyoutPresenter))
        {
            BasedOn = (Style)Application.Current.Resources["KynxaFlyoutPresenterStyle"]
        };
        presenter.Setters.Add(new Setter(FrameworkElement.WidthProperty, 280d));
        presenter.Setters.Add(new Setter(Control.PaddingProperty, new Thickness(14)));
        presenter.Setters.Add(new Setter(AutomationProperties.AutomationIdProperty, "UsageStatisticsFlyout"));
        _flyout.FlyoutPresenterStyle = presenter;
        _flyout.Content = _details;
        BuildDetails();
        _flyout.Opened += (_, _) => _chevron.Glyph = "\uE70D";
        _flyout.Closed += (_, _) => _chevron.Glyph = "\uE70E";
        Loaded += OnLoaded;
        Unloaded += OnUnloaded;
        ApplyAppearance();
        RefreshLabels();
        RenderValues(false);
    }

    public void Update(UsageDisplayState state)
    {
        ArgumentNullException.ThrowIfNull(state);
        if (_disposed) return;
        if (!DispatcherQueue.HasThreadAccess)
        {
            DispatcherQueue.TryEnqueue(() => Update(state));
            return;
        }
        _state = state;
        RefreshLabels();
        RenderValues(true);
    }

    private static TextBlock Label(string text = "") => new()
    {
        Text = text,
        FontSize = 12,
        VerticalAlignment = VerticalAlignment.Center,
        TextWrapping = TextWrapping.NoWrap
    };

    private void BuildDetails()
    {
        var heading = new Grid { Margin = new Thickness(0, 0, 0, 3), ColumnSpacing = 10 };
        heading.ColumnDefinitions.Add(new() { Width = new GridLength(1, GridUnitType.Star) });
        heading.ColumnDefinitions.Add(new() { Width = GridLength.Auto });
        _detailScope.FontSize = 13;
        _detailScope.FontWeight = FontWeights.Medium;
        heading.Children.Add(_detailScope);
        var detailTitle = Label();
        UiLocalization.SetText(detailTitle, "用量明细");
        _secondaryLabels.Add(detailTitle);
        Grid.SetColumn(detailTitle, 1);
        heading.Children.Add(detailTitle);
        _details.Children.Add(heading);
        AddDetailRow("输入 Token", _detailInput);
        AddDetailRow("输出 Token", _detailOutput);
        AddDetailRow("缓存命中 Token", _detailHit);
        _description.TextWrapping = TextWrapping.Wrap;
        _description.FontSize = 11;
        _description.Margin = new Thickness(0, 4, 0, 0);
        _details.Children.Add(_description);
        var divider = new Border { Height = 1, Margin = new Thickness(0, 4, 0, 0), Background = AppearanceService.GetBrush("KynxaDividerBrush") };
        _details.Children.Add(divider);
        _floating.HorizontalAlignment = HorizontalAlignment.Stretch;
        _floating.HorizontalContentAlignment = HorizontalAlignment.Left;
        _floating.Style = (Style)Application.Current.Resources["KynxaQuietButtonStyle"];
        _floating.Padding = new Thickness(5, 7, 5, 7);
        _floating.FontSize = 12;
        UiLocalization.SetContent(_floating, "桌面悬浮显示");
        UiLocalization.SetAutomationName(_floating, "桌面悬浮显示");
        AutomationProperties.SetAutomationId(_floating, "UsageStatisticsFloatingAction");
        _floating.Click += (_, _) =>
        {
            _flyout.Hide();
            FloatingRequested?.Invoke(this, EventArgs.Empty);
        };
        _details.Children.Add(_floating);
    }

    private void AddDetailRow(string key, TextBlock value)
    {
        var row = new Grid { ColumnSpacing = 12, Margin = new Thickness(0, 1, 0, 1) };
        row.ColumnDefinitions.Add(new() { Width = new GridLength(1, GridUnitType.Star) });
        row.ColumnDefinitions.Add(new() { Width = GridLength.Auto });
        var label = Label();
        UiLocalization.SetText(label, key);
        _secondaryLabels.Add(label);
        row.Children.Add(label);
        value.FontWeight = FontWeights.Medium;
        value.IsTextSelectionEnabled = true;
        Grid.SetColumn(value, 1);
        row.Children.Add(value);
        _details.Children.Add(row);
    }

    private void ToggleDetails(object sender, RoutedEventArgs args)
    {
        if (_disposed) return;
        if (_flyout.IsOpen) _flyout.Hide();
        else _flyout.ShowAt(_button);
    }

    private void RefreshLabels()
    {
        if (_disposed) return;
        _scope.Text = _state.ScopeLabel;
        _scope.Visibility = IsCompact ? Visibility.Collapsed : Visibility.Visible;
        _unit.Text = "Token";
        _unit.Visibility = IsCompact ? Visibility.Collapsed : Visibility.Visible;
        _inline.Spacing = IsCompact ? 4 : 6;
        _cacheLabel.Text = IsCompact ? UiText.Get("命中") : _state.CacheLabel;
        _button.MinWidth = IsCompact ? 190 : 280;
        _detailScope.Text = _state.ScopeLabel;
        AutomationProperties.SetName(_details, UiText.Get("用量明细"));
    }

    private void RenderValues(bool animate)
    {
        _total.Show(_state.TotalText, animate);
        _cache.Show(_state.CacheText, animate);
        _detailInput.Text = _state.InputText;
        _detailOutput.Text = _state.OutputText;
        _detailHit.Text = _state.CacheHitText;
        string description = string.IsNullOrWhiteSpace(_state.Description) && _state.IsIncomplete
            ? UiText.Get("部分用量未知，已知数值仅供参考。") : _state.Description;
        _description.Text = description;
        _description.Visibility = string.IsNullOrWhiteSpace(description) ? Visibility.Collapsed : Visibility.Visible;
        AutomationProperties.SetName(_button, $"{_state.ScopeLabel}，{_state.TotalLabel} {_state.TotalText}，{_state.CacheLabel} {_state.CacheText}");
        ToolTipService.SetToolTip(_button, string.IsNullOrWhiteSpace(description)
            ? UiText.Get("点击展开用量明细") : description);
    }

    private void ApplyAppearance()
    {
        var font = (FontFamily)Application.Current.Resources["KynxaUIFont"];
        var secondary = AppearanceService.GetBrush("KynxaSecondaryTextBrush");
        var text = AppearanceService.GetBrush("KynxaTextBrush");
        foreach (var label in new[] { _scope, _unit, _dot, _cacheLabel, _description }.Concat(_secondaryLabels))
        {
            label.FontFamily = font;
            label.Foreground = secondary;
        }
        foreach (var label in new[] { _detailScope, _detailInput, _detailOutput, _detailHit })
        {
            label.FontFamily = font;
            label.Foreground = text;
        }
        _button.FontFamily = _floating.FontFamily = font;
        _button.Foreground = _floating.Foreground = text;
        _chevron.Foreground = secondary;
        _total.SetAppearance(font, text);
        _cache.SetAppearance(font, AppearanceService.GetBrush("KynxaAccentTextBrush"));
        _details.RequestedTheme = AppearanceService.Current.IsDark ? ElementTheme.Dark : ElementTheme.Light;
    }

    private void OnLoaded(object sender, RoutedEventArgs args)
    {
        if (_disposed || _subscribed) return;
        _subscribed = true;
        AppearanceService.Changed += AppearanceChanged;
        UiText.LanguageChanged += LanguageChanged;
        ApplyAppearance();
        RefreshLabels();
    }

    private void AppearanceChanged(object? sender, EventArgs args)
    {
        if (_disposed || !_subscribed) return;
        if (DispatcherQueue.HasThreadAccess) ApplyAppearance();
        else DispatcherQueue.TryEnqueue(() => { if (!_disposed && _subscribed) ApplyAppearance(); });
    }

    private void LanguageChanged(object? sender, EventArgs args)
    {
        if (_disposed || !_subscribed) return;
        void Refresh()
        {
            if (_disposed || !_subscribed) return;
            RefreshLabels();
            RenderValues(false);
        }
        if (DispatcherQueue.HasThreadAccess) Refresh();
        else DispatcherQueue.TryEnqueue(Refresh);
    }

    private void OnUnloaded(object sender, RoutedEventArgs args) => StopObserving();

    private void StopObserving()
    {
        if (_subscribed)
        {
            _subscribed = false;
            AppearanceService.Changed -= AppearanceChanged;
            UiText.LanguageChanged -= LanguageChanged;
        }
        _flyout.Hide();
        _total.StopAnimations();
        _cache.StopAnimations();
    }

    public void Dispose()
    {
        if (_disposed) return;
        _disposed = true;
        StopObserving();
        FloatingRequested = null;
    }
}

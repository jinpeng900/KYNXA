using System.Globalization;
using System.Numerics;
using KYNXA.Contracts;
using KYNXA_Desktop.Services;
using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Automation;
using Microsoft.UI.Xaml.Controls;
using Microsoft.UI.Xaml.Media;

namespace KYNXA_Desktop.Controls;

/// <summary>
/// A compact usage row with inline details; it never opens a flyout or a separate window.
/// 紧凑用量行在原位置展开详情，不打开浮出层或独立窗口。
/// </summary>
public sealed class ConversationUsageSummary : UserControl
{
    private readonly Border _surface = new() { CornerRadius = new CornerRadius(12), BorderThickness = new Thickness(1) };
    private readonly Button _header = new() { HorizontalAlignment = HorizontalAlignment.Stretch,
        HorizontalContentAlignment = HorizontalAlignment.Stretch, CornerRadius = new(9),
        Padding = new Thickness(8, 5, 8, 5), MinWidth = 0, Background = new SolidColorBrush(Microsoft.UI.Colors.Transparent),
        BorderThickness = new Thickness(0) };
    private readonly TextBlock _scope = new() { FontSize = 11, VerticalAlignment = VerticalAlignment.Center };
    private readonly TextBlock _summary = new() { FontSize = 11, TextTrimming = TextTrimming.CharacterEllipsis,
        VerticalAlignment = VerticalAlignment.Center, HorizontalAlignment = HorizontalAlignment.Right };
    private readonly FontIcon _chevron = new() { Glyph = "\uE70D", FontSize = 9, VerticalAlignment = VerticalAlignment.Center };
    private readonly StackPanel _details = new() { Spacing = 5, Margin = new Thickness(8, 2, 8, 8), Visibility = Visibility.Collapsed };
    private readonly TextBlock _counts = Detail();
    private readonly TextBlock _cache = Detail();
    private readonly TextBlock _calls = Detail();
    private readonly TextBlock _notice = Detail();
    private ConversationUsageSnapshot? _snapshot;
    private bool _unavailable;

    public bool IsDetailsExpanded => _details.Visibility == Visibility.Visible;
    public event EventHandler? DetailsChanged;

    public ConversationUsageSummary()
    {
        if (Application.Current.Resources.TryGetValue("KynxaQuietButtonStyle", out var quiet) && quiet is Style style)
            _header.Style = style;
        HorizontalContentAlignment = HorizontalAlignment.Stretch;
        VerticalContentAlignment = VerticalAlignment.Stretch;
        var content = new StackPanel();
        var row = new Grid { ColumnSpacing = 8 };
        row.ColumnDefinitions.Add(new() { Width = GridLength.Auto });
        row.ColumnDefinitions.Add(new() { Width = new GridLength(1, GridUnitType.Star) });
        row.ColumnDefinitions.Add(new() { Width = GridLength.Auto });
        row.Children.Add(_scope); Grid.SetColumn(_summary, 1); row.Children.Add(_summary);
        Grid.SetColumn(_chevron, 2); row.Children.Add(_chevron); _header.Content = row;
        _header.Click += (_, _) =>
        {
            _details.Visibility = IsDetailsExpanded ? Visibility.Collapsed : Visibility.Visible;
            Render(); DetailsChanged?.Invoke(this, EventArgs.Empty);
        };
        content.Children.Add(_header);
        foreach (var detail in new[] { _counts, _cache, _calls, _notice }) _details.Children.Add(detail);
        content.Children.Add(new ScrollViewer { Content = _details, MaxHeight = 168,
            HorizontalScrollBarVisibility = ScrollBarVisibility.Disabled, VerticalScrollBarVisibility = ScrollBarVisibility.Auto });
        _surface.Child = content; Content = _surface;
        AutomationProperties.SetAutomationId(this, "ConversationUsageSummary");
        AutomationProperties.SetAutomationId(_header, "ConversationUsageDetailsToggle");
        Loaded += (_, _) => { UiText.LanguageChanged += PresentationChanged; AppearanceService.Changed += PresentationChanged; Render(); };
        Unloaded += (_, _) => { UiText.LanguageChanged -= PresentationChanged; AppearanceService.Changed -= PresentationChanged; };
        Render();
    }

    public void Reset()
    { _snapshot = null; _unavailable = false; _details.Visibility = Visibility.Collapsed; Render(); }

    public void Show(ConversationUsageSnapshot snapshot)
    { _snapshot = snapshot; _unavailable = false; Render(); }

    public void ShowUnavailable() { _unavailable = true; Render(); }

    private static TextBlock Detail() => new() { FontSize = 11, TextWrapping = TextWrapping.Wrap };
    private static string Count(ulong value) => value.ToString("N0", CultureInfo.CurrentCulture);
    private static string Compact(BigInteger value) => value >= 1_000_000
        ? ((double)value / 1_000_000).ToString("0.#", CultureInfo.CurrentCulture) + "M"
        : value >= 10_000 ? ((double)value / 1_000).ToString("0.#", CultureInfo.CurrentCulture) + "K"
        : value.ToString("N0", CultureInfo.CurrentCulture);

    private void PresentationChanged(object? sender, EventArgs args) => DispatcherQueue.TryEnqueue(Render);
    private void Render()
    {
        _surface.Background = AppearanceService.GetBrush("KynxaReplySurfaceBrush");
        _surface.BorderBrush = AppearanceService.GetBrush("KynxaDividerBrush");
        _header.Foreground = _scope.Foreground = _summary.Foreground = _chevron.Foreground = AppearanceService.GetBrush("KynxaSecondaryTextBrush");
        _header.FocusVisualPrimaryBrush = AppearanceService.GetBrush("KynxaSecondaryTextBrush");
        _header.FocusVisualSecondaryBrush = new SolidColorBrush(Microsoft.UI.Colors.Transparent);
        foreach (var line in new[] { _counts, _cache, _calls, _notice }) line.Foreground = AppearanceService.GetBrush("KynxaSecondaryTextBrush");
        _scope.Text = UiText.Get("本对话用量");
        _chevron.Glyph = IsDetailsExpanded ? "\uE70E" : "\uE70D";
        AutomationProperties.SetName(_header, UiText.Get(IsDetailsExpanded ? "收起对话用量" : "展开对话用量"));
        if (_snapshot is not { } snapshot)
        {
            _summary.Text = UiText.Get("Token — · 缓存 —");
            _counts.Text = UiText.Get(_unavailable ? "暂时无法读取用量，可稍后重试。" : "正在读取用量…");
            _cache.Text = _calls.Text = string.Empty;
            _notice.Text = UiText.Get("仅统计当前对话及其子任务；部分供应商不返回缓存数据。");
            return;
        }
        bool unknown = _unavailable || snapshot.UnknownTokenRequests > 0 || snapshot.PendingRequests > 0;
        string total = Compact((BigInteger)snapshot.InputTokens + snapshot.OutputTokens) + (unknown ? "+?" : "");
        string rate = snapshot.CacheHitRate is double hit ? hit.ToString("P0", CultureInfo.CurrentCulture) : "—";
        bool partialCache = snapshot.KnownCacheRequests > 0 && (snapshot.KnownCacheRequests < snapshot.ModelRequests || snapshot.PendingRequests > 0);
        if (partialCache && snapshot.CacheHitRate is not null)
            rate = string.Format(CultureInfo.CurrentCulture, UiText.Get("{0}（已知）"), rate);
        _summary.Text = string.Format(CultureInfo.CurrentCulture, UiText.Get("Token {0} · 缓存 {1}"), total, rate);
        _counts.Text = string.Format(CultureInfo.CurrentCulture, UiText.Get("输入 {0} · 输出 {1}"),
            Count(snapshot.InputTokens) + (unknown ? "+?" : ""), Count(snapshot.OutputTokens) + (unknown ? "+?" : ""));
        _cache.Text = snapshot.KnownCacheRequests > 0
            ? string.Format(CultureInfo.CurrentCulture, UiText.Get("已知缓存命中 {0} / 对应输入 {1}"), Count(snapshot.CacheReadTokens), Count(snapshot.CacheInputTokens))
            : UiText.Get("缓存数据尚不可用。");
        _calls.Text = string.Format(CultureInfo.CurrentCulture, UiText.Get("模型请求 {0} · 用量完整 {1} · 待结算 {2}"),
            Count(snapshot.ModelRequests), Count(snapshot.KnownTokenRequests), Count(snapshot.PendingRequests));
        _notice.Text = UiText.Get(_unavailable ? "暂时无法读取用量，可稍后重试。"
            : unknown ? "部分用量未知，已知数值仅供参考。"
            : partialCache ? "缓存率仅按已返回缓存字段的请求计算。" : "仅统计当前对话及其子任务；部分供应商不返回缓存数据。");
        AutomationProperties.SetName(this, _scope.Text + " · " + _summary.Text);
    }
}

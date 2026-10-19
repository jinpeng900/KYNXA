using KYNXA_Desktop.Services;
using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Automation;
using Microsoft.UI.Xaml.Controls;
using Microsoft.UI.Xaml.Media;

namespace KYNXA_Desktop.Views;

public sealed partial class ShellPage
{
    private bool _observingUsage;

    private void InitializeUsageMonitor()
    {
        Loaded += (_, _) =>
        {
            if (!_observingUsage)
            {
                _observingUsage = true;
                UsageMonitorService.Changed += UsageChanged;
                UsageStatistics.FloatingRequested += ShowFloatingUsage;
            }
            UsageStatistics.Update(UsageMonitorService.Display);
        };
        Unloaded += (_, _) =>
        {
            _observingUsage = false;
            UsageMonitorService.Changed -= UsageChanged;
            UsageStatistics.FloatingRequested -= ShowFloatingUsage;
        };
    }

    private void UsageChanged(object? sender, EventArgs args) => UsageStatistics.Update(UsageMonitorService.Display);

    private void UsageFooter_SizeChanged(object sender, SizeChangedEventArgs args) =>
        UsageStatistics.IsCompact = args.NewSize.Width < 600;

    private async void ShowFloatingUsage(object? sender, EventArgs args)
    {
        await UsageMonitorService.SetFloatingEnabledAsync(true);
        // Errors are also exposed in the usage details and Settings notice.
        // 启动错误同时呈现在用量明细及设置提示中。
        UsageStatistics.Update(UsageMonitorService.Display);
    }

    private FrameworkElement CreateUsageMonitorSettings(FontFamily font)
    {
        var root = new StackPanel { Spacing = 6 };
        var row = new Grid { ColumnSpacing = 14, Padding = new Thickness(14, 10, 14, 10),
            CornerRadius = new CornerRadius(12), Background = AppearanceService.GetBrush("KynxaSettingsCardBrush") };
        row.ColumnDefinitions.Add(new() { Width = new GridLength(1, GridUnitType.Star) });
        row.ColumnDefinitions.Add(new() { Width = GridLength.Auto });
        var labels = new StackPanel { Spacing = 4, VerticalAlignment = VerticalAlignment.Center };
        var title = new TextBlock { FontFamily = font, FontSize = 14, Foreground = AppearanceService.GetBrush("KynxaTextBrush") };
        UiLocalization.SetText(title, "桌面悬浮统计卡");
        var description = new TextBlock { FontFamily = font, FontSize = 12, TextWrapping = TextWrapping.Wrap,
            Foreground = AppearanceService.GetBrush("KynxaSecondaryTextBrush") };
        UiLocalization.SetText(description, "内置统计始终显示；可额外开启桌面卡片，累计数据保持一致。");
        labels.Children.Add(title); labels.Children.Add(description);
        row.Children.Add(labels);
        var toggle = new ToggleSwitch { IsOn = UsageMonitorService.FloatingEnabled, OnContent = "", OffContent = "",
            VerticalAlignment = VerticalAlignment.Center, MinWidth = 48 };
        UiLocalization.SetAutomationName(toggle, "桌面悬浮统计卡");
        AutomationProperties.SetAutomationId(toggle, "UsageMonitorFloatingToggle");
        Grid.SetColumn(toggle, 1); row.Children.Add(toggle);
        var notice = new InfoBar { IsOpen = UsageMonitorService.LastFloatingError is not null, IsClosable = true,
            Message = UsageMonitorService.LastFloatingError ?? "", Severity = InfoBarSeverity.Warning };
        AutomationProperties.SetAutomationId(notice, "UsageMonitorSettingsNotice");
        bool applying = false;
        void Refresh(object? sender, EventArgs args)
        {
            if (applying) return;
            applying = true;
            toggle.IsOn = UsageMonitorService.FloatingEnabled;
            notice.Message = UsageMonitorService.LastFloatingError ?? "";
            notice.IsOpen = UsageMonitorService.LastFloatingError is not null;
            applying = false;
        }
        root.Loaded += (_, _) => { UsageMonitorService.Changed += Refresh; Refresh(null, EventArgs.Empty); };
        root.Unloaded += (_, _) => UsageMonitorService.Changed -= Refresh;
        toggle.Toggled += async (_, _) =>
        {
            if (applying) return;
            applying = true;
            toggle.IsEnabled = false;
            try
            {
                string? error = await UsageMonitorService.SetFloatingEnabledAsync(toggle.IsOn);
                notice.Message = error ?? "";
                notice.IsOpen = error is not null;
                toggle.IsOn = UsageMonitorService.FloatingEnabled;
            }
            finally { toggle.IsEnabled = true; applying = false; }
        };
        root.Children.Add(row); root.Children.Add(notice);
        return root;
    }
}

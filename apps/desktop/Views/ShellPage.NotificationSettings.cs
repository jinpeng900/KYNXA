using KYNXA_Desktop.Services;
using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Automation;
using Microsoft.UI.Xaml.Controls;
using Microsoft.UI.Xaml.Media;

namespace KYNXA_Desktop.Views;

public sealed partial class ShellPage
{
    private Action? _refreshCompletionNotificationSettings;

    private FrameworkElement BuildCompletionNotificationSettingsSection(Window window)
    {
        var font = (FontFamily)Application.Current.Resources["KynxaUIFont"];
        var content = new StackPanel { Spacing = 4 };
        TextBlock Label(string key, bool secondary)
        {
            var text = new TextBlock { FontFamily = font, FontSize = secondary ? 12 : 14,
                Foreground = AppearanceService.GetBrush(secondary ? "KynxaSecondaryTextBrush" : "KynxaTextBrush"),
                TextWrapping = TextWrapping.Wrap };
            UiLocalization.Bind(text, TextBlock.TextProperty, key);
            return text;
        }
        content.Children.Add(Label("回复完成通知", secondary: false));
        content.Children.Add(Label("回复完成后通过系统通知提醒，点击通知可返回应用。", secondary: true));
        var toggle = new ToggleSwitch { IsOn = ExpansionNotifications.Enabled, MinWidth = 0,
            OnContent = null, OffContent = null, VerticalAlignment = VerticalAlignment.Center };
        UiLocalization.Bind(toggle, AutomationProperties.NameProperty, "回复完成通知");
        UiLocalization.Bind(toggle, AutomationProperties.HelpTextProperty, "回复完成后通过系统通知提醒，点击通知可返回应用。");
        AutomationProperties.SetAutomationId(toggle, "ReplyCompletionNotificationToggle");
        var row = new Grid { ColumnSpacing = 20 };
        row.ColumnDefinitions.Add(new ColumnDefinition { Width = new GridLength(1, GridUnitType.Star) });
        row.ColumnDefinitions.Add(new ColumnDefinition { Width = GridLength.Auto });
        Grid.SetColumn(toggle, 1);
        row.Children.Add(content);
        row.Children.Add(toggle);
        var error = new InfoBar { IsOpen = false, IsClosable = false, Severity = InfoBarSeverity.Error };
        UiLocalization.Bind(error, InfoBar.MessageProperty, "通知偏好未能保存，请稍后重试。");
        AutomationProperties.SetAutomationId(error, "ReplyCompletionNotificationError");
        bool refreshing = false;
        void Refresh()
        {
            refreshing = true;
            try { toggle.IsOn = ExpansionNotifications.Enabled; }
            finally { refreshing = false; }
            error.IsOpen = false;
        }
        toggle.Toggled += (_, _) =>
        {
            if (!refreshing) error.IsOpen = !TrySetCompletionNotifications(toggle.IsOn);
        };
        // The workbench, navigation menu and settings use the same preference and live card.
        // 工作台、导航菜单和设置共用同一偏好及当前卡片，不复制布局或重置既有开关。
        _refreshCompletionNotificationSettings = Refresh;
        window.Closed += (_, _) =>
        {
            if (_refreshCompletionNotificationSettings == Refresh) _refreshCompletionNotificationSettings = null;
        };
        var body = new StackPanel { Spacing = 10 };
        body.Children.Add(row);
        body.Children.Add(error);
        var card = new Border { Padding = new Thickness(16), CornerRadius = new CornerRadius(12),
            Background = AppearanceService.GetBrush("KynxaSettingsCardBrush"), Child = body };
        AutomationProperties.SetAutomationId(card, "ReplyCompletionNotificationSettings");
        return card;
    }
}

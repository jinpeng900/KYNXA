using KYNXA_Desktop.Models.UI;
using KYNXA_Desktop.Services;
using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Automation;
using Microsoft.UI.Xaml.Controls;
using Microsoft.UI.Xaml.Media;

namespace KYNXA_Desktop.Views;

public sealed partial class ShellPage
{
    private Action? _refreshMainWindowCloseSettings;

    internal MainWindowClosePreferences GetMainWindowClosePreferences() => new(
        MainWindowClosePreferences.NormalizeAction(_layout.MainWindowCloseAction), _layout.ConfirmMainWindowClose);

    internal bool TrySetMainWindowClosePreferences(MainWindowClosePreferences preferences)
    {
        var selected = preferences with { Action = MainWindowClosePreferences.NormalizeAction(preferences.Action) };
        if (GetMainWindowClosePreferences() == selected)
        {
            _refreshMainWindowCloseSettings?.Invoke();
            return true;
        }
        string previousAction = _layout.MainWindowCloseAction;
        bool previousConfirm = _layout.ConfirmMainWindowClose;
        // Both the settings card and close confirmation save the same live layout; stale copies must not overwrite it.
        // 设置卡片与关闭确认保存同一份当前布局，避免独立加载的旧副本覆盖其他偏好。
        _layout.MainWindowCloseAction = selected.Action;
        _layout.ConfirmMainWindowClose = selected.Confirm;
        if (!_layoutStateService.Save(_layout))
        {
            _layout.MainWindowCloseAction = previousAction;
            _layout.ConfirmMainWindowClose = previousConfirm;
            return false;
        }
        _refreshMainWindowCloseSettings?.Invoke();
        return true;
    }

    private FrameworkElement BuildMainWindowCloseSettingsSection(Window window)
    {
        var preferences = GetMainWindowClosePreferences();
        var body = new StackPanel { Spacing = 12 };
        var heading = MainWindowCloseLabel("关闭主窗口时", 14);
        heading.FontWeight = Microsoft.UI.Text.FontWeights.SemiBold;
        body.Children.Add(heading);
        body.Children.Add(BuildMainWindowCloseOptions(preferences.Action, "MainWindowCloseSettings", out var tray, out var exit));
        var ask = new CheckBox { IsChecked = preferences.Confirm, MinWidth = 0 };
        UiLocalization.Bind(ask, ContentControl.ContentProperty, "关闭时询问");
        UiLocalization.Bind(ask, AutomationProperties.NameProperty, "关闭时询问");
        AutomationProperties.SetAutomationId(ask, "MainWindowCloseAsk");
        body.Children.Add(ask);
        body.Children.Add(MainWindowCloseLabel("可随时在“通用”设置中更改。", 12, secondary: true));
        var error = new InfoBar { IsOpen = false, IsClosable = false, Severity = InfoBarSeverity.Error };
        UiLocalization.Bind(error, InfoBar.MessageProperty, "关闭偏好未能保存，请稍后重试。");
        AutomationProperties.SetAutomationId(error, "MainWindowCloseSettingsError");
        body.Children.Add(error);
        bool refreshing = false;
        void Refresh()
        {
            refreshing = true;
            try
            {
                var current = GetMainWindowClosePreferences();
                tray.IsChecked = current.Action == MainWindowClosePreferences.Tray;
                exit.IsChecked = current.Action == MainWindowClosePreferences.Exit;
                ask.IsChecked = current.Confirm;
            }
            finally { refreshing = false; }
        }
        void Save()
        {
            if (refreshing) return;
            var selected = new MainWindowClosePreferences(exit.IsChecked == true ? MainWindowClosePreferences.Exit : MainWindowClosePreferences.Tray,
                ask.IsChecked == true);
            if (TrySetMainWindowClosePreferences(selected)) error.IsOpen = false;
            else
            {
                Refresh();
                error.IsOpen = true;
            }
        }
        tray.Checked += (_, _) => Save();
        exit.Checked += (_, _) => Save();
        ask.Checked += (_, _) => Save();
        ask.Unchecked += (_, _) => Save();
        Action refresh = () => { Refresh(); error.IsOpen = false; };
        _refreshMainWindowCloseSettings = refresh;
        window.Closed += (_, _) =>
        {
            if (ReferenceEquals(_refreshMainWindowCloseSettings, refresh)) _refreshMainWindowCloseSettings = null;
        };
        var card = new Border { Padding = new Thickness(16), CornerRadius = new CornerRadius(12),
            Background = AppearanceService.GetBrush("KynxaSettingsCardBrush"), Child = body };
        AutomationProperties.SetAutomationId(card, "MainWindowCloseSettings");
        return card;
    }

    private static StackPanel BuildMainWindowCloseOptions(string action, string automationPrefix,
        out RadioButton tray, out RadioButton exit)
    {
        string group = automationPrefix + Guid.NewGuid().ToString("N");
        RadioButton Option(string key, string labelKey, string descriptionKey, string suffix)
        {
            var content = new StackPanel { Spacing = 3 };
            content.Children.Add(MainWindowCloseLabel(labelKey, 14));
            content.Children.Add(MainWindowCloseLabel(descriptionKey, 12, secondary: true));
            var radio = new RadioButton { GroupName = group, Tag = key, Content = content, MinWidth = 0,
                HorizontalAlignment = HorizontalAlignment.Stretch, HorizontalContentAlignment = HorizontalAlignment.Stretch,
                Padding = new Thickness(8, 4, 0, 4), IsChecked = action == key };
            var transparent = new SolidColorBrush(Microsoft.UI.Colors.Transparent);
            var uncheckedFill = new SolidColorBrush(Microsoft.UI.Colors.Transparent);
            // Match the SDK's pointer-over glyph size and reuse idle brushes so hovering changes neither size nor color.
            // 与 SDK 的悬停圆点尺寸一致，并复用静止画刷，悬停时不再改变大小或颜色。
            // Native press, focus and disabled states remain available; mutable shared brushes still follow the theme.
            // 原生按下、焦点及禁用状态继续保留，共享画刷仍随主题原位更新。
            radio.Resources["RadioButtonCheckGlyphSize"] = 14d;
            foreach ((string prefix, Brush brush) in new (string, Brush)[]
            {
                ("RadioButtonForeground", AppearanceService.GetBrush("KynxaTextBrush")),
                ("RadioButtonBackground", transparent),
                ("RadioButtonBorderBrush", transparent),
                ("RadioButtonOuterEllipseStroke", AppearanceService.GetBrush("KynxaSecondaryTextBrush")),
                ("RadioButtonOuterEllipseCheckedStroke", AppearanceService.GetBrush("KynxaAccentBrush")),
                ("RadioButtonOuterEllipseCheckedFill", AppearanceService.GetBrush("KynxaAccentBrush")),
                ("RadioButtonCheckGlyphFill", AppearanceService.GetBrush("KynxaOnAccentBrush")),
                ("RadioButtonCheckGlyphStroke", transparent),
                ("RadioButtonCheckGlyphStrokeChecked", transparent)
            })
                foreach (string state in new[] { string.Empty, "PointerOver", "Pressed" })
                    radio.Resources[prefix + state] = brush;
            radio.Resources["RadioButtonOuterEllipseFillPointerOver"] = uncheckedFill;
            radio.Resources["RadioButtonOuterEllipseFillPressed"] = uncheckedFill;
            static SolidColorBrush? FindUncheckedFill(DependencyObject element)
            {
                if (element is Microsoft.UI.Xaml.Shapes.Ellipse { Name: "OuterEllipse", Fill: SolidColorBrush fill }) return fill;
                for (int index = 0; index < VisualTreeHelper.GetChildrenCount(element); index++)
                    if (FindUncheckedFill(VisualTreeHelper.GetChild(element, index)) is { } found) return found;
                return null;
            }
            void RefreshUncheckedFill()
            {
                if (!radio.IsLoaded || !radio.IsEnabled || VisualTreeHelper.GetChildrenCount(radio) == 0 ||
                    VisualTreeHelper.GetChild(radio, 0) is not FrameworkElement root) return;
                string state = VisualStateManager.GetVisualStateGroups(root)
                    .FirstOrDefault(group => group.Name == "CommonStates")?.CurrentState?.Name ?? "Normal";
                if (state == "Disabled") return;
                bool restoreState = state != "Normal";
                if (restoreState && !VisualStateManager.GoToState(radio, "Normal", false)) return;
                try
                {
                    // The SDK owns the normal fill, including its theme-specific alpha; recolor the existing state brush.
                    // 普通填充及随主题变化的透明度由 SDK 决定；原位更新已有状态画刷，避免悬停切换底色。
                    if (FindUncheckedFill(root) is { } idleFill && !ReferenceEquals(idleFill, uncheckedFill))
                    {
                        uncheckedFill.Color = idleFill.Color;
                        uncheckedFill.Opacity = idleFill.Opacity;
                    }
                }
                finally { if (restoreState) VisualStateManager.GoToState(radio, state, false); }
            }
            void ThemeChanged(FrameworkElement sender, object args)
            {
                if (radio.IsLoaded) radio.DispatcherQueue.TryEnqueue(RefreshUncheckedFill);
            }
            radio.Loaded += (_, _) =>
            {
                radio.ActualThemeChanged -= ThemeChanged;
                radio.ActualThemeChanged += ThemeChanged;
                RefreshUncheckedFill();
            };
            radio.Unloaded += (_, _) => radio.ActualThemeChanged -= ThemeChanged;
            AutomationProperties.SetAutomationId(radio, automationPrefix + suffix);
            UiLocalization.Bind(radio, AutomationProperties.NameProperty, labelKey);
            return radio;
        }
        tray = Option(MainWindowClosePreferences.Tray, "最小化到系统托盘", "窗口隐藏到托盘，正在进行的任务继续运行。", "Tray");
        exit = Option(MainWindowClosePreferences.Exit, "退出 KYNXA", "退出应用，正在运行的任务按现有退出流程暂停。", "Exit");
        var options = new StackPanel { Spacing = 6 };
        options.Children.Add(tray);
        options.Children.Add(exit);
        return options;
    }

    private static TextBlock MainWindowCloseLabel(string key, double fontSize, bool secondary = false)
    {
        var label = new TextBlock { FontFamily = (FontFamily)Application.Current.Resources["KynxaUIFont"], FontSize = fontSize,
            Foreground = AppearanceService.GetBrush(secondary ? "KynxaSecondaryTextBrush" : "KynxaTextBrush"),
            TextWrapping = TextWrapping.Wrap };
        UiLocalization.Bind(label, TextBlock.TextProperty, key);
        return label;
    }
}

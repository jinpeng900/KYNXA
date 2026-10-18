using System.Runtime.CompilerServices;
using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Controls;
using Microsoft.UI.Xaml.Media;
using Windows.Foundation;

namespace KYNXA_Desktop.Services;

/// <summary>
/// Keeps native modal dialogs on their owner's theme and the application's shared palette.
/// 让原生模态对话框沿用所属窗口主题和应用共享配色。
/// </summary>
public static class DialogPresentation
{
    private static readonly ConditionalWeakTable<ContentDialog, AppearanceBinding> Bindings = new();

    public static ContentDialog Prepare(ContentDialog dialog)
    {
        ArgumentNullException.ThrowIfNull(dialog);
        Bindings.GetValue(dialog, static owner => new AppearanceBinding(owner)).Apply();
        return dialog;
    }

    public static IAsyncOperation<ContentDialogResult> ShowAsync(ContentDialog dialog) => Prepare(dialog).ShowAsync();

    private sealed class AppearanceBinding
    {
        private readonly ContentDialog _dialog;
        private FrameworkElement? _owner;
        private Grid? _commandSpace;
        private (string? Primary, string? Secondary, string? Close) _actionTexts;
        private CenteredAction? _centeredAction;
        private bool _open;

        private sealed record CenteredAction(Button Button, int Column, int ColumnSpan,
            HorizontalAlignment Alignment, double MinWidth, double MinHeight, double MaxWidth,
            Thickness Padding, int CharacterSpacing, DataTemplate? ContentTemplate);

        public AppearanceBinding(ContentDialog dialog)
        {
            _dialog = dialog;
            ConfigureResources();
            _dialog.Opened += Opened;
            _dialog.Closed += Closed;
        }

        public void Apply()
        {
            var owner = _dialog.XamlRoot?.Content as FrameworkElement;
            _dialog.RequestedTheme = owner?.RequestedTheme is ElementTheme.Light or ElementTheme.Dark
                ? owner.RequestedTheme : owner?.ActualTheme ?? (AppearanceService.Current.IsDark ? ElementTheme.Dark : ElementTheme.Light);
            if (_open) UpdateActionLayout();
        }

        private void ActionLayoutUpdated(object? sender, object args)
        {
            // Recheck after action changes or a native template rebuild; ordinary layout passes keep the cached result.
            // 操作变化或原生模板重建后重新核对；普通布局过程沿用缓存，避免逐帧遍历模板。
            var actionTexts = (_dialog.PrimaryButtonText, _dialog.SecondaryButtonText, _dialog.CloseButtonText);
            if (_commandSpace?.IsLoaded == true && actionTexts == _actionTexts) return;
            UpdateActionLayout();
        }

        private void UpdateActionLayout()
        {
            _commandSpace = FindElement<Grid>(_dialog, "CommandSpace");
            if (_commandSpace is null) return;
            _actionTexts = (_dialog.PrimaryButtonText, _dialog.SecondaryButtonText, _dialog.CloseButtonText);
            var actions = _commandSpace.Children.OfType<Button>().Where(button => button.Visibility == Visibility.Visible).ToArray();
            if (actions.Length != 1 || _commandSpace.ColumnDefinitions.Count == 0)
            {
                RestoreActionLayout();
                return;
            }
            var action = actions[0];
            if (!ReferenceEquals(_centeredAction?.Button, action))
            {
                RestoreActionLayout();
                _centeredAction = new(action, Grid.GetColumn(action), Grid.GetColumnSpan(action), action.HorizontalAlignment,
                    action.MinWidth, action.MinHeight, action.MaxWidth, action.Padding, action.CharacterSpacing, action.ContentTemplate);
            }
            // The SDK keeps an unused star column when only Close is visible; span the whole command area.
            // SDK 仅显示关闭按钮时仍保留闲置星号列；让单个操作跨越整个命令区域，保证左右对称。
            Grid.SetColumn(action, 0);
            Grid.SetColumnSpan(action, _commandSpace.ColumnDefinitions.Count);
            action.HorizontalAlignment = HorizontalAlignment.Center;
            // Keep short acknowledgements comfortably sized while allowing localized text to grow within the native cap.
            // 简短确认按钮保留适中的点击范围，本地化文字可在原生宽度上限内自适应。
            action.MinWidth = Math.Max(_centeredAction!.MinWidth, (double)Application.Current.Resources["KynxaDialogActionMinWidth"]);
            action.MinHeight = Math.Max(_centeredAction.MinHeight, (double)Application.Current.Resources["KynxaDialogActionMinHeight"]);
            action.Padding = (Thickness)Application.Current.Resources["KynxaDialogActionPadding"];
            // Native tracking scales with the font; keep the original label intact for localization and automation.
            // 原生字间距随字体缩放；保留原始文字，避免插入空格影响本地化和辅助功能读取。
            bool isAcknowledgement = action.Content is string text && text == "知道了";
            action.CharacterSpacing = isAcknowledgement
                ? (int)Application.Current.Resources["KynxaDialogAcknowledgementCharacterSpacing"] : _centeredAction.CharacterSpacing;
            action.ContentTemplate = isAcknowledgement
                ? (DataTemplate)Application.Current.Resources["KynxaDialogAcknowledgementTemplate"] : _centeredAction.ContentTemplate;
            double maximumWidth = Application.Current.Resources.TryGetValue("ContentDialogButtonMaxWidth", out var resource) && resource is double width
                ? width : 202;
            action.MaxWidth = Math.Min(_centeredAction!.MaxWidth, Math.Max(action.MinWidth, maximumWidth));
        }

        private void RestoreActionLayout()
        {
            if (_centeredAction is not { } original) return;
            Grid.SetColumn(original.Button, original.Column);
            Grid.SetColumnSpan(original.Button, original.ColumnSpan);
            original.Button.HorizontalAlignment = original.Alignment;
            original.Button.MinWidth = original.MinWidth;
            original.Button.MinHeight = original.MinHeight;
            original.Button.MaxWidth = original.MaxWidth;
            original.Button.Padding = original.Padding;
            original.Button.CharacterSpacing = original.CharacterSpacing;
            original.Button.ContentTemplate = original.ContentTemplate;
            _centeredAction = null;
        }

        private static T? FindElement<T>(DependencyObject root, string name) where T : FrameworkElement
        {
            if (root is T element && element.Name == name) return element;
            for (int index = 0; index < VisualTreeHelper.GetChildrenCount(root); index++)
                if (FindElement<T>(VisualTreeHelper.GetChild(root, index), name) is { } child) return child;
            return null;
        }

        private void ConfigureResources()
        {
            var resources = Application.Current.Resources;
            _dialog.Background = AppearanceService.GetBrush("KynxaMainBrush");
            _dialog.Foreground = AppearanceService.GetBrush("KynxaTextBrush");
            _dialog.BorderBrush = AppearanceService.GetBrush("KynxaDividerBrush");
            _dialog.BorderThickness = new Thickness(1);
            _dialog.CornerRadius = (CornerRadius)resources["KynxaMenuRadius"];
            _dialog.PrimaryButtonStyle = _dialog.SecondaryButtonStyle = _dialog.CloseButtonStyle =
                (Style)resources["KynxaDialogButtonStyle"];

            // Register shared brushes once; their colors update in place while this dialog is open.
            // 共享画刷只注册一次；打开期间原位更新颜色，避免把同一资源反复挂入字典。
            // WinUI's inner clipping grid and its default action style resolve these local resources.
            // WinUI 内层裁剪网格和默认操作样式从此处解析资源，必须与外层圆角及应用按钮配色一致。
            _dialog.Resources["OverlayCornerRadius"] = _dialog.CornerRadius;
            _dialog.Resources["ControlCornerRadius"] = resources["KynxaControlRadius"];
            _dialog.Resources["AccentButtonStyle"] = resources["KynxaDialogAccentButtonStyle"];
            SetBrush("ContentDialogBackground", "KynxaMainBrush");
            SetBrush("ContentDialogTopOverlay", "KynxaMainBrush");
            SetBrush("ContentDialogForeground", "KynxaTextBrush");
            SetBrush("ContentDialogBorderBrush", "KynxaDividerBrush");
            SetBrush("ContentDialogSeparatorBorderBrush", "KynxaDividerBrush");
            SetBrush("SystemControlFocusVisualPrimaryBrush", "KynxaFocusBrush");
            SetButtonBrushes("Button", "KynxaSettingsCardBrush", "KynxaControlHoverBrush", "KynxaControlPressedBrush", "KynxaTextBrush");
            SetButtonBrushes("AccentButton", "KynxaSendBrush", "KynxaSendHoverBrush", "KynxaSendPressedBrush", "KynxaOnAccentBrush");
        }

        private void SetButtonBrushes(string prefix, string background, string hover, string pressed, string foreground)
        {
            SetBrush(prefix + "Background", background);
            SetBrush(prefix + "BackgroundPointerOver", hover);
            SetBrush(prefix + "BackgroundPressed", pressed);
            SetBrush(prefix + "BackgroundDisabled", "KynxaSendDisabledBrush");
            foreach (string suffix in new[] { string.Empty, "PointerOver", "Pressed" })
            {
                SetBrush(prefix + "Foreground" + suffix, foreground);
                SetBrush(prefix + "BorderBrush" + suffix, "KynxaDividerBrush");
            }
            SetBrush(prefix + "ForegroundDisabled", "KynxaDisabledTextBrush");
            SetBrush(prefix + "BorderBrushDisabled", "KynxaDividerBrush");
        }

        private void SetBrush(string resourceKey, string paletteKey) =>
            _dialog.Resources[resourceKey] = AppearanceService.GetBrush(paletteKey);

        private void Opened(ContentDialog sender, ContentDialogOpenedEventArgs args)
        {
            if (_open) return;
            _open = true;
            _owner = _dialog.XamlRoot?.Content as FrameworkElement;
            AppearanceService.Changed += AppearanceChanged;
            _dialog.LayoutUpdated += ActionLayoutUpdated;
            if (_owner is not null) _owner.ActualThemeChanged += OwnerThemeChanged;
            Apply();
        }

        private void Closed(ContentDialog sender, ContentDialogClosedEventArgs args)
        {
            _open = false;
            AppearanceService.Changed -= AppearanceChanged;
            _dialog.LayoutUpdated -= ActionLayoutUpdated;
            RestoreActionLayout();
            _commandSpace = null;
            if (_owner is not null) _owner.ActualThemeChanged -= OwnerThemeChanged;
            _owner = null;
        }

        private void AppearanceChanged(object? sender, EventArgs args)
        {
            // A queued change must not retain or update a dialog after its modal lifetime ends.
            // 排队的主题更新不能在模态生命周期结束后继续更新或持有对话框。
            if (!_open) return;
            if (_dialog.DispatcherQueue.HasThreadAccess) Apply();
            else _dialog.DispatcherQueue.TryEnqueue(() => { if (_open) Apply(); });
        }

        private void OwnerThemeChanged(FrameworkElement sender, object args) => AppearanceChanged(sender, EventArgs.Empty);
    }
}

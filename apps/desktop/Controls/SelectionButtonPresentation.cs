using KYNXA_Desktop.Services;
using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Controls;
using Microsoft.UI.Xaml.Media;

namespace KYNXA_Desktop.Controls;

/// <summary>
/// Keeps selection visible while native button states provide shared themed pointer feedback.
/// 保持原生按钮的选中底色，并为未选中按钮提供统一的主题悬停与按下反馈。
/// </summary>
internal static class SelectionButtonPresentation
{
    private static readonly Brush TransparentBrush = new SolidColorBrush(Microsoft.UI.Colors.Transparent);

    public static void Apply(Button button, bool selected, Brush? idleBackground = null)
    {
        string? pointerState = VisualTreeHelper.GetChildrenCount(button) > 0 && VisualTreeHelper.GetChild(button, 0) is FrameworkElement root
            ? VisualStateManager.GetVisualStateGroups(root).FirstOrDefault(group => group.Name == "CommonStates")?.CurrentState?.Name
            : null;
        bool highContrastSelection = selected && AppearanceService.Current.IsHighContrast;
        var selection = AppearanceService.GetBrush(highContrastSelection ? "KynxaAccentBrush" : "KynxaSelectionBrush");
        var foreground = AppearanceService.GetBrush(highContrastSelection ? "KynxaOnAccentBrush" : "KynxaTextBrush");
        button.Background = selected ? selection : idleBackground ?? TransparentBrush;
        button.Foreground = foreground;
        // Native templates otherwise replace a local Background when the pointer enters the selected button.
        // 原生模板会在指针进入时覆盖按钮的本地 Background，因此同时配置选中项的状态资源。
        UpdateStateBrush(button, "ButtonBackgroundPointerOver", selected ? selection : AppearanceService.GetBrush("KynxaControlHoverBrush"));
        UpdateStateBrush(button, "ButtonBackgroundPressed", selected ? selection : AppearanceService.GetBrush("KynxaControlPressedBrush"));
        UpdateStateBrush(button, "ButtonForegroundPointerOver", foreground);
        UpdateStateBrush(button, "ButtonForegroundPressed", foreground);
        // A changed TemplateBinding can replace the active setter's fill without changing its state name.
        // TemplateBinding 更新可能覆盖当前状态的底色，但不改变状态名；同一帧无动画重应用，保留真实指针反馈。
        if (pointerState is "PointerOver" or "Pressed")
        {
            VisualStateManager.GoToState(button, "Normal", false);
            VisualStateManager.GoToState(button, pointerState, false);
        }
    }

    internal static void UpdateStateBrush(FrameworkElement control, string resourceKey, SolidColorBrush source)
    {
        // An active WinUI setter retains its brush reference when a resource is replaced. Recolor an owned brush instead.
        // WinUI 已生效的状态 Setter 会保留原画刷引用；原位改色，确保指针未离开时切换选择也立即生效。
        string ownedKey = "KynxaOwnedState_" + resourceKey;
        if (!control.Resources.TryGetValue(ownedKey, out var resource) || resource is not SolidColorBrush brush)
        {
            brush = new SolidColorBrush(source.Color);
            control.Resources[ownedKey] = brush;
            control.Resources[resourceKey] = brush;
        }
        else brush.Color = source.Color;
    }
}

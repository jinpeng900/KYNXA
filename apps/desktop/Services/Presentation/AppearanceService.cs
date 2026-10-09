using System.Globalization;
using KYNXA_Desktop.Models.UI;
using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Controls;
using Microsoft.UI.Xaml.Media;
using Windows.UI;

namespace KYNXA_Desktop.Services;

/// <summary>
/// Applies local UI palettes without replacing controls, conversations or brush references.
/// 应用本机界面配色，不替换控件、会话或画刷引用。
/// </summary>
public static class AppearanceService
{
    public const string DefaultPaletteId = AppearancePalette.DefaultId;
    public static IReadOnlyList<AppearancePalette> Palettes { get; } = Array.AsReadOnly(new[]
    {
        new AppearancePalette("classic-gray", "经典灰", "#262626", "#404040", "#171717",
            "#F0EFED", "#FBFAF9", "#FFFFFF", "#DDDDDD", "#DDDDDD"),
        new AppearancePalette(DefaultPaletteId, "雾蓝", "#4568B2", "#395A9E", "#304D8B",
            "#EDF2FC", "#F5F7FB", "#FFFFFF", "#DCE3EF", "#D3DFF6"),
        new AppearancePalette("teal", "青绿", "#287A68", "#226A5A", "#1B594B",
            "#EAF5F0", "#F4F8F6", "#FFFFFF", "#D7E7DF", "#CDE8DD"),
        new AppearancePalette("violet", "柔紫", "#7653AA", "#664593", "#573A80",
            "#F2EDFA", "#F7F5FA", "#FFFFFF", "#E3DBED", "#E2D7F1"),
        new AppearancePalette("sand", "暖砂", "#8B653A", "#795730", "#664928",
            "#F6EFE5", "#F9F7F3", "#FFFEFC", "#E7DED0", "#EBDDCA")
    });

    public static AppearancePalette Current { get; private set; } = Palettes[1];
    public static event EventHandler? Changed;

    public static string NormalizePaletteId(string? id) =>
        Palettes.FirstOrDefault(palette => palette.Id == id)?.Id ?? DefaultPaletteId;

    public static Color ParseColor(string hex)
    {
        uint rgb = uint.Parse(hex.AsSpan(1), NumberStyles.HexNumber, CultureInfo.InvariantCulture);
        return Color.FromArgb(255, (byte)(rgb >> 16), (byte)(rgb >> 8), (byte)rgb);
    }

    public static SolidColorBrush GetBrush(string key) => (SolidColorBrush)Application.Current.Resources[key];

    public static void Apply(string? id)
    {
        Current = Palettes.First(palette => palette.Id == NormalizePaletteId(id));
        var palette = Current;
        SetBrush("KynxaTitleBarBrush", palette.Sidebar);
        SetBrush("KynxaSidebarBrush", palette.Sidebar);
        SetBrush("KynxaMainBrush", palette.Main);
        SetBrush("KynxaSurfaceBrush", palette.Main);
        SetBrush("KynxaTextBrush", palette.Text);
        SetBrush("KynxaIconBrush", palette.Text);
        SetBrush("KynxaSecondaryTextBrush", palette.Secondary);
        SetBrush("KynxaDividerBrush", palette.Border);
        SetBrush("KynxaComposerBorderBrush", palette.Border);
        SetBrush("KynxaSendBrush", palette.Accent);
        SetBrush("KynxaSendHoverBrush", palette.AccentHover);
        SetBrush("KynxaSendPressedBrush", palette.AccentPressed);
        SetBrush("KynxaAccentBrush", palette.Accent);
        SetBrush("KynxaFocusBrush", palette.Accent);
        SetBrush("KynxaSelectionBrush", palette.Soft);
        SetBrush("KynxaSettingsCardBrush", palette.Sidebar);
        SetBrush("KynxaSegmentIdleBrush", palette.Sidebar);
        SetBrush("KynxaComposerFooterBrush", palette.Sidebar);
        SetBrush("KynxaComposerActionBrush", palette.Sidebar);
        SetBrush("KynxaAmbientWaveBrush", palette.Soft);
        SetBrush("KynxaAmbientWaveSoftBrush", palette.Sidebar);
        // WinUI controls use these resources for focus, selection and primary actions.
        // WinUI 控件通过这些资源统一焦点、选中状态和主要操作。
        foreach (string key in AccentBrushKeys) SetBrush(key, palette.Accent);
        SetBrush("AccentFillColorSecondaryBrush", palette.AccentHover);
        SetBrush("AccentFillColorTertiaryBrush", palette.AccentPressed);
        // Native TextBox uses white selected text; its background must remain dark enough.
        // 原生 TextBox 的选中文字为白色，选区背景必须保留足够对比度。
        SetBrush("TextControlSelectionHighlightColor", palette.Accent);
        foreach (string key in new[] { "ComboBoxItemBorderBrushSelected", "ComboBoxItemBorderBrushSelectedPointerOver", "ComboBoxItemBorderBrushSelectedPressed" })
            SetBrush(key, palette.Soft);
        Changed?.Invoke(null, EventArgs.Empty);
    }

    private static readonly string[] AccentBrushKeys =
    [
        "AccentFillColorDefaultBrush", "AccentTextFillColorPrimaryBrush", "AccentTextFillColorSecondaryBrush",
        "SystemControlHighlightAccentBrush", "SystemControlFocusVisualPrimaryBrush",
        "TextControlBorderBrushFocused", "ComboBoxBackgroundBorderBrushFocused", "ComboBoxBorderBrushFocused", "ComboBoxItemPillFillBrush",
        "ListViewItemSelectionIndicatorBrush", "PivotHeaderItemSelectedPipeFill", "PivotHeaderItemFocusPipeFill",
        "CheckBoxCheckBackgroundStrokeChecked", "CheckBoxCheckBackgroundStrokeCheckedPointerOver", "CheckBoxCheckBackgroundStrokeCheckedPressed",
        "CheckBoxCheckBackgroundFillChecked", "CheckBoxCheckBackgroundFillCheckedPointerOver",
        "CheckBoxCheckBackgroundFillCheckedPressed", "CheckBoxCheckBorderBrushChecked",
        "CheckBoxCheckBorderBrushCheckedPointerOver", "CheckBoxCheckBorderBrushCheckedPressed"
    ];

    private static void SetBrush(string key, string hex)
    {
        var resources = Application.Current.Resources;
        // Mutate existing brushes: StaticResource consumers retain the same object.
        // 修改原画刷的颜色，让 StaticResource 使用者继续持有同一个对象。
        if (resources.TryGetValue(key, out object value) && value is SolidColorBrush brush)
            brush.Color = ParseColor(hex);
        else resources[key] = new SolidColorBrush(ParseColor(hex));
    }

    public static void TrackWindow(Window window)
    {
        bool isClosed = false;
        void Update(object? sender, EventArgs args)
        {
            if (isClosed) return;
            if (window.Content is not FrameworkElement root) return;
            if (!root.DispatcherQueue.HasThreadAccess)
            {
                root.DispatcherQueue.TryEnqueue(() => Update(sender, args));
                return;
            }
            if (root is Panel panel) panel.Background = GetBrush("KynxaMainBrush");
            if (root is Control control) control.Foreground = GetBrush("KynxaTextBrush");
            if (!Microsoft.UI.Windowing.AppWindowTitleBar.IsCustomizationSupported()) return;
            var titleBar = window.AppWindow.TitleBar;
            titleBar.BackgroundColor = ParseColor(Current.Sidebar);
            titleBar.InactiveBackgroundColor = ParseColor(Current.Sidebar);
            titleBar.ForegroundColor = ParseColor(Current.Text);
            titleBar.ButtonBackgroundColor = Microsoft.UI.Colors.Transparent;
            titleBar.ButtonInactiveBackgroundColor = Microsoft.UI.Colors.Transparent;
            titleBar.ButtonForegroundColor = ParseColor(Current.Text);
            titleBar.ButtonHoverBackgroundColor = ParseColor(Current.Soft);
            titleBar.ButtonHoverForegroundColor = ParseColor(Current.Text);
            titleBar.ButtonPressedBackgroundColor = ParseColor(Current.Selection);
            titleBar.ButtonPressedForegroundColor = ParseColor(Current.Text);
        }
        Changed += Update;
        window.Closed += (_, _) => { isClosed = true; Changed -= Update; };
        Update(null, EventArgs.Empty);
    }
}

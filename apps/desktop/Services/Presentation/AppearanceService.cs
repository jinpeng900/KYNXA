using System.Globalization;
using System.Runtime.InteropServices;
using KYNXA_Desktop.Models.UI;
using Microsoft.UI.Dispatching;
using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Controls;
using Microsoft.UI.Xaml.Media;
using Windows.UI;
using Windows.UI.ViewManagement;

namespace KYNXA_Desktop.Services;

/// <summary>
/// Applies local UI palettes without replacing controls, conversations or brush references.
/// 应用本机界面配色，不替换控件、会话或画刷引用。
/// </summary>
public static class AppearanceService
{
    public const string DefaultPaletteId = AppearancePalette.DefaultId;
    public const string DefaultMode = AppearancePalette.DefaultMode;
    public const double DefaultChatFontSize = AppearancePalette.DefaultChatFontSize;
    public const double MinimumChatFontSize = AppearancePalette.MinimumChatFontSize;
    public const double MaximumChatFontSize = AppearancePalette.MaximumChatFontSize;
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
    public static string Mode { get; private set; } = DefaultMode;
    public static double ChatFontSize { get; private set; } = DefaultChatFontSize;
    public static string? CustomAccent { get; private set; }
    public static event EventHandler? Changed;

    private static readonly HashSet<Window> TrackedWindows = new(ReferenceEqualityComparer.Instance);
    private static readonly Dictionary<string, SolidColorBrush> OwnedBrushes = new(StringComparer.Ordinal);
    private static DispatcherQueue? UiDispatcher;
    private static UISettings? SystemSettings;
    private static AccessibilitySettings? Accessibility;
    private static bool SystemAppearanceRefreshQueued;
    private const uint SettingChangedMessage = 0x001A;
    private const uint SystemColorsChangedMessage = 0x0015;
    private const uint ThemeChangedMessage = 0x031A;
    private const uint NonClientDestroyMessage = 0x0082;
    private static readonly nuint AppearanceSubclassId = 0x4B594E58;
    // Keep the delegate alive while any native HWND can invoke it.
    // 只要原生 HWND 仍可能回调，就必须保持委托存活。
    private static readonly WindowSubclassProc AppearanceSubclassCallback = OnWindowMessage;

    public static string NormalizePaletteId(string? id) =>
        Palettes.FirstOrDefault(palette => palette.Id == id)?.Id ?? DefaultPaletteId;

    public static string NormalizeMode(string? mode) => mode is "dark" or "system" ? mode : DefaultMode;

    public static double NormalizeChatFontSize(double size) => double.IsFinite(size)
        ? Math.Round(Math.Clamp(size, MinimumChatFontSize, MaximumChatFontSize) * 2, MidpointRounding.AwayFromZero) / 2
        : DefaultChatFontSize;

    public static string? NormalizeAccent(string? accent)
    {
        string? value = accent?.Trim();
        return value is { Length: 7 } && value[0] == '#' &&
            uint.TryParse(value.AsSpan(1), NumberStyles.AllowHexSpecifier, CultureInfo.InvariantCulture, out _)
            ? value.ToUpperInvariant() : null;
    }

    public static Color ParseColor(string hex)
    {
        uint rgb = uint.Parse(hex.AsSpan(1), NumberStyles.HexNumber, CultureInfo.InvariantCulture);
        return Color.FromArgb(255, (byte)(rgb >> 16), (byte)(rgb >> 8), (byte)rgb);
    }

    public static SolidColorBrush GetBrush(string key) => (SolidColorBrush)Application.Current.Resources[key];

    public static void Apply(string? id) => ApplyPreferences(id, Mode, ChatFontSize, CustomAccent);

    public static void ApplyPreferences(string? paletteId, string? mode, double chatFontSize, string? accent)
    {
        UiDispatcher ??= DispatcherQueue.GetForCurrentThread();
        if (UiDispatcher is { HasThreadAccess: false } dispatcher)
        {
            dispatcher.TryEnqueue(() => ApplyPreferences(paletteId, mode, chatFontSize, accent));
            return;
        }
        EnsureSystemSettings();
        Mode = NormalizeMode(mode);
        ChatFontSize = NormalizeChatFontSize(chatFontSize);
        CustomAccent = NormalizeAccent(accent);
        var preset = Palettes.First(palette => palette.Id == NormalizePaletteId(paletteId));
        ApplyEffectivePalette(preset);
    }

    private static void EnsureSystemSettings()
    {
        // Both WinRT change events are unsupported in desktop apps; observe the owned HWNDs instead.
        // 这两个 WinRT 变更事件均不支持桌面应用；改为监听应用自己的 HWND 消息。
        SystemSettings ??= new UISettings();
        Accessibility ??= new AccessibilitySettings();
    }

    private static void QueueSystemAppearance()
    {
        if (SystemAppearanceRefreshQueued || UiDispatcher is not { } dispatcher) return;
        SystemAppearanceRefreshQueued = true;
        // Resolve outside the native callback, after Windows has processed its theme notification.
        // 脱离原生回调后解析，确保 Windows 已处理主题通知，也避免托管异常跨越原生回调边界。
        if (!dispatcher.TryEnqueue(() =>
        {
            SystemAppearanceRefreshQueued = false;
            if (Mode != "system" && Accessibility?.HighContrast != true && !Current.IsHighContrast) return;
            // A queued system notification always resolves the latest saved user preference.
            // 排队中的系统通知始终解析用户当前偏好，不能恢复已经过期的主题选择。
            var preset = Palettes.First(palette => palette.Id == Current.Id);
            ApplyEffectivePalette(preset);
        })) SystemAppearanceRefreshQueued = false;
    }

    private static nint OnWindowMessage(nint window, uint message, nuint wParam, nint lParam,
        nuint subclassId, nuint referenceData)
    {
        if (message is SettingChangedMessage or SystemColorsChangedMessage or ThemeChangedMessage)
            QueueSystemAppearance();
        else if (message == NonClientDestroyMessage)
            RemoveWindowSubclass(window, AppearanceSubclassCallback, AppearanceSubclassId);
        return DefSubclassProc(window, message, wParam, lParam);
    }

    private static void ApplyEffectivePalette(AppearancePalette preset)
    {
        bool systemDark = SystemSettings is not null &&
            Luminance(SystemSettings.GetColorValue(UIColorType.Background)) < 0.5;
        bool dark = Mode == "dark" || Mode == "system" && systemDark;
        Current = Accessibility?.HighContrast == true && SystemSettings is not null
            ? ResolveHighContrastPalette(preset, SystemSettings.UIElementColor)
            : ResolvePalette(preset, dark, CustomAccent);
        var palette = Current;
        SetBrush("KynxaTitleBarBrush", palette.Sidebar);
        SetBrush("KynxaSidebarBrush", palette.Sidebar);
        SetBrush("KynxaMainBrush", palette.Main);
        SetBrush("KynxaSurfaceBrush", palette.Main);
        SetBrush("KynxaTextBrush", palette.Text);
        SetBrush("KynxaIconBrush", palette.Text);
        SetBrush("KynxaSecondaryTextBrush", palette.Secondary);
        SetBrush("KynxaMutedTextBrush", palette.Muted);
        SetBrush("KynxaOnAccentBrush", palette.OnAccent);
        SetBrush("KynxaAccentTextBrush", palette.AccentText);
        SetBrush("KynxaSuccessBrush", palette.Success);
        SetBrush("KynxaErrorBrush", palette.Error);
        SetBrush("KynxaWarningBrush", palette.Warning);
        SetBrush("KynxaDividerBrush", palette.Border);
        SetBrush("KynxaComposerBorderBrush", palette.Border);
        SetBrush("KynxaSendBrush", palette.Accent);
        SetBrush("KynxaSendHoverBrush", palette.AccentHover);
        SetBrush("KynxaSendPressedBrush", palette.AccentPressed);
        SetBrush("KynxaSendDisabledBrush", palette.Disabled);
        SetBrush("KynxaDisabledTextBrush", palette.DisabledText);
        SetBrush("KynxaAccentBrush", palette.Accent);
        SetBrush("KynxaFocusBrush", palette.AccentText);
        SetBrush("KynxaSelectionBrush", palette.Soft);
        SetBrush("KynxaSettingsCardBrush", palette.Sidebar);
        SetBrush("KynxaSegmentIdleBrush", palette.Sidebar);
        SetBrush("KynxaComposerFooterBrush", palette.Sidebar);
        SetBrush("KynxaComposerActionBrush", palette.Sidebar);
        SetBrush("KynxaAmbientWaveBrush", palette.Soft);
        SetBrush("KynxaAmbientWaveSoftBrush", palette.Sidebar);
        SetBrush("KynxaReplySurfaceBrush", palette.ReplySurface);
        SetBrush("KynxaControlHoverBrush", palette.ControlHover);
        SetBrush("KynxaControlPressedBrush", palette.ControlPressed);
        SetBrush("ButtonBackgroundPointerOver", palette.ControlHover);
        SetBrush("ButtonBackgroundPressed", palette.ControlPressed);
        SetBrush("KynxaWorkPanelDividerBrush", palette.Border);
        SetBrush("KynxaPermissionFullBrush", palette.Warning);
        SetBrush("KynxaOnlineBrush", palette.Success);
        // WinUI controls use these resources for focus, selection and primary actions.
        // WinUI 控件通过这些资源统一焦点、选中状态和主要操作。
        foreach (string key in AccentBrushKeys) SetBrush(key, palette.Accent);
        SetBrush("AccentTextFillColorPrimaryBrush", palette.AccentText);
        SetBrush("AccentTextFillColorSecondaryBrush", palette.AccentText);
        SetBrush("SystemControlFocusVisualPrimaryBrush", palette.AccentText);
        SetBrush("AccentFillColorSecondaryBrush", palette.AccentHover);
        SetBrush("AccentFillColorTertiaryBrush", palette.AccentPressed);
        // Standard native selection uses white text; high contrast keeps the OS highlight pair.
        // 普通原生选区使用白字；高对比度保留系统选区前景与背景的配对。
        SetBrush("TextControlSelectionHighlightColor", palette.Accent);
        SetBrush("TextOnAccentFillColorPrimaryBrush", palette.OnAccent);
        foreach (string key in new[] { "ComboBoxItemBorderBrushSelected", "ComboBoxItemBorderBrushSelectedPointerOver", "ComboBoxItemBorderBrushSelectedPressed" })
            SetBrush(key, palette.Soft);
        Changed?.Invoke(null, EventArgs.Empty);
    }

    internal static AppearancePalette ResolvePalette(AppearancePalette preset, bool dark, string? customAccent)
    {
        customAccent = NormalizeAccent(customAccent);
        string accent = customAccent ?? preset.Accent;
        // Button fill and linked text serve different contrast pairs, especially in dark mode.
        // 按钮填充与链接文字对应不同的对比度组合，深色模式尤其不能共用同一个色调。
        accent = EnsureContrast(accent, "#FFFFFF", lighten: false);
        if (!dark)
        {
            string soft = customAccent is null ? preset.Soft : Blend(preset.Main, accent, 0.1);
            return preset with
            {
                Accent = accent,
                AccentHover = customAccent is null ? preset.AccentHover : Blend(accent, "#000000", 0.12),
                AccentPressed = customAccent is null ? preset.AccentPressed : Blend(accent, "#000000", 0.22),
                AccentText = EnsureContrast(accent, soft, lighten: false),
                Soft = soft,
                Selection = customAccent is null ? preset.Selection : Blend(preset.Main, accent, 0.22),
                Muted = EnsureContrast("#6B7280", soft, lighten: false),
                ReplySurface = preset.Sidebar,
                ControlHover = Blend(preset.Main, preset.Text, 0.045),
                ControlPressed = Blend(preset.Main, preset.Text, 0.08)
            };
        }

        const string main = "#181B21";
        const string sidebar = "#20242C";
        const string text = "#E8EBF0";
        string darkSoft = Blend(sidebar, customAccent ?? preset.Accent, 0.22);
        string textBackground = Luminance(ParseColor(darkSoft)) > Luminance(ParseColor(sidebar)) ? darkSoft : sidebar;
        return preset with
        {
            IsDark = true, Main = main, Sidebar = sidebar, Soft = darkSoft,
            Border = "#46505E", Selection = Blend(sidebar, "#8FB4FF", 0.32),
            Text = EnsureContrast(text, textBackground, lighten: true),
            Secondary = EnsureContrast("#B6BEC9", textBackground, lighten: true),
            Muted = EnsureContrast("#A5AEBB", textBackground, lighten: true),
            Accent = accent, AccentHover = Blend(accent, "#000000", 0.1),
            AccentPressed = Blend(accent, "#000000", 0.18),
            AccentText = EnsureContrast(customAccent ?? preset.Accent, textBackground, lighten: true),
            Success = EnsureContrast("#82C8A0", textBackground, lighten: true),
            Error = EnsureContrast("#F2A59D", textBackground, lighten: true),
            Warning = EnsureContrast("#EAC574", textBackground, lighten: true),
            ReplySurface = sidebar, ControlHover = Blend(main, text, 0.08),
            ControlPressed = Blend(main, text, 0.13), Disabled = "#3B414C", DisabledText = "#A5AEBB",
            CodeKeyword = "#C6A2F5", CodeString = "#9EC8F2", CodeComment = "#A0BB8A",
            CodeNumber = "#8FBDF8", CodeType = "#88C6A2", CodeFunction = "#E0B090"
        };
    }

    internal static AppearancePalette ResolveHighContrastPalette(AppearancePalette preset, Func<UIElementType, Color> readColor)
    {
        string Hex(UIElementType type) => ToHex(readColor(type));
        string main = Hex(UIElementType.Window);
        string text = Hex(UIElementType.WindowText);
        string highlight = Hex(UIElementType.Highlight);
        string highlightText = Hex(UIElementType.HighlightText);
        string link = Hex(UIElementType.Hotlight);
        // High contrast honors the OS scheme, and never persists its temporary override as a custom palette.
        // 高对比度尊重系统方案，临时覆盖不写回用户保存的自定义配色。
        return preset with
        {
            IsDark = Luminance(ParseColor(main)) < 0.5, IsHighContrast = true,
            Main = main, Sidebar = main, Soft = main, ReplySurface = main,
            Text = text, Secondary = text, Muted = text, Border = text,
            Accent = highlight, AccentHover = highlight, AccentPressed = highlight,
            AccentText = link, OnAccent = highlightText, Selection = highlight,
            Success = text, Error = text, Warning = text,
            ControlHover = main, ControlPressed = main,
            Disabled = Hex(UIElementType.ButtonFace), DisabledText = Hex(UIElementType.GrayText),
            CodeKeyword = text, CodeString = text, CodeComment = text,
            CodeNumber = text, CodeType = text, CodeFunction = text
        };
    }

    private static string ToHex(Color color) => $"#{color.R:X2}{color.G:X2}{color.B:X2}";

    private static string Blend(string background, string foreground, double opacity)
    {
        Color first = ParseColor(background), second = ParseColor(foreground);
        byte Channel(byte start, byte end) => (byte)Math.Round(start + (end - start) * opacity);
        return ToHex(Color.FromArgb(255, Channel(first.R, second.R), Channel(first.G, second.G), Channel(first.B, second.B)));
    }

    private static double Luminance(Color color)
    {
        static double Linear(byte channel)
        {
            double value = channel / 255d;
            return value <= 0.04045 ? value / 12.92 : Math.Pow((value + 0.055) / 1.055, 2.4);
        }
        return 0.2126 * Linear(color.R) + 0.7152 * Linear(color.G) + 0.0722 * Linear(color.B);
    }

    internal static double ContrastRatio(string first, string second)
    {
        double left = Luminance(ParseColor(first)), right = Luminance(ParseColor(second));
        return (Math.Max(left, right) + 0.05) / (Math.Min(left, right) + 0.05);
    }

    private static string EnsureContrast(string color, string background, bool lighten)
    {
        if (ContrastRatio(color, background) >= 4.5) return color;
        string destination = lighten ? "#FFFFFF" : "#000000";
        for (int step = 1; step <= 100; step++)
        {
            string adjusted = Blend(color, destination, step / 100d);
            if (ContrastRatio(adjusted, background) >= 4.5) return adjusted;
        }
        return destination;
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
        if (!OwnedBrushes.TryGetValue(key, out var brush))
        {
            var resources = Application.Current.Resources;
            brush = FindApplicationBrush(resources, key);
            if (brush is null)
            {
                // SDK resources belong to its theme dictionaries; register our own override only once.
                // SDK 资源属于其主题字典；首次创建应用自己的覆盖画刷，之后不再重复插入。
                brush = new SolidColorBrush(ParseColor(hex));
                try { resources[key] = brush; }
                catch (Exception error) when (error is COMException or ArgumentException)
                {
                    throw new InvalidOperationException($"Cannot register appearance brush '{key}' in application resources.", error);
                }
            }
            OwnedBrushes.Add(key, brush);
        }
        // Mutate application-owned brushes so existing StaticResource references remain valid.
        // 只更新应用自有画刷的颜色，保留现有 StaticResource 引用，不重复挂载同一个 XAML 对象。
        try { brush.Color = ParseColor(hex); }
        catch (COMException error)
        {
            throw new InvalidOperationException($"Cannot update the color of appearance brush '{key}'.", error);
        }
    }

    private static SolidColorBrush? FindApplicationBrush(ResourceDictionary resources, string key)
    {
        // Enumeration examines local entries; TryGetValue also resolves SDK theme resources we must not reparent.
        // 枚举检查字典本地条目；TryGetValue 还会解析 SDK 主题资源，不能将那些对象重新挂到应用字典。
        foreach (var resource in resources)
            if (Equals(resource.Key, key) && resource.Value is SolidColorBrush applicationBrush) return applicationBrush;
        foreach (var merged in resources.MergedDictionaries.Reverse())
        {
            if (merged is XamlControlsResources) continue;
            if (FindApplicationBrush(merged, key) is { } mergedBrush) return mergedBrush;
        }
        return null;
    }

    public static void TrackWindow(Window window)
    {
        if (!window.DispatcherQueue.HasThreadAccess)
        {
            window.DispatcherQueue.TryEnqueue(() => TrackWindow(window));
            return;
        }
        if (!TrackedWindows.Add(window)) return;
        UiDispatcher ??= window.DispatcherQueue;
        EnsureSystemSettings();
        nint windowHandle = WinRT.Interop.WindowNative.GetWindowHandle(window);
        if (windowHandle == 0 || !SetWindowSubclass(windowHandle, AppearanceSubclassCallback, AppearanceSubclassId, 0))
        {
            TrackedWindows.Remove(window);
            throw new InvalidOperationException("Cannot register system appearance notifications for the owned window.");
        }
        bool isClosed = false;
        void Activated(object sender, WindowActivatedEventArgs args) => QueueSystemAppearance();
        void Update(object? sender, EventArgs args)
        {
            if (isClosed) return;
            if (window.Content is not FrameworkElement root) return;
            if (!root.DispatcherQueue.HasThreadAccess)
            {
                root.DispatcherQueue.TryEnqueue(() => Update(sender, args));
                return;
            }
            root.RequestedTheme = Current.IsDark ? ElementTheme.Dark : ElementTheme.Light;
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
            titleBar.ButtonInactiveForegroundColor = ParseColor(Current.Secondary);
            titleBar.ButtonHoverBackgroundColor = ParseColor(Current.Soft);
            titleBar.ButtonHoverForegroundColor = ParseColor(Current.Text);
            titleBar.ButtonPressedBackgroundColor = ParseColor(Current.Selection);
            titleBar.ButtonPressedForegroundColor = ParseColor(Current.IsHighContrast ? Current.OnAccent : Current.Text);
        }
        Changed += Update;
        window.Activated += Activated;
        window.Closed += (_, _) =>
        {
            isClosed = true;
            Changed -= Update;
            window.Activated -= Activated;
            TrackedWindows.Remove(window);
            RemoveWindowSubclass(windowHandle, AppearanceSubclassCallback, AppearanceSubclassId);
        };
        Update(null, EventArgs.Empty);
        QueueSystemAppearance();
    }

    [UnmanagedFunctionPointer(CallingConvention.Winapi)]
    private delegate nint WindowSubclassProc(nint window, uint message, nuint wParam, nint lParam,
        nuint subclassId, nuint referenceData);

    [DllImport("comctl32.dll", ExactSpelling = true)]
    [return: MarshalAs(UnmanagedType.Bool)]
    private static extern bool SetWindowSubclass(nint window, WindowSubclassProc callback, nuint subclassId, nuint referenceData);

    [DllImport("comctl32.dll", ExactSpelling = true)]
    [return: MarshalAs(UnmanagedType.Bool)]
    private static extern bool RemoveWindowSubclass(nint window, WindowSubclassProc callback, nuint subclassId);

    [DllImport("comctl32.dll", ExactSpelling = true)]
    private static extern nint DefSubclassProc(nint window, uint message, nuint wParam, nint lParam);
}

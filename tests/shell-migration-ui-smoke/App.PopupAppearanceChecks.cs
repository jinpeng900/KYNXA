using System.Reflection;
using System.Runtime.InteropServices;
using System.Text.Json;
using KYNXA_Desktop.Models.UI;
using KYNXA_Desktop.Services;
using MemoryUiSmoke;
using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Controls;
using Microsoft.UI.Xaml.Controls.Primitives;
using Microsoft.UI.Xaml.Input;
using Microsoft.UI.Xaml.Media;
using Microsoft.UI.Xaml.Media.Imaging;
using Rectangle = Microsoft.UI.Xaml.Shapes.Rectangle;
using Windows.Foundation;
using Windows.Graphics.Imaging;
using Windows.Storage.Streams;
using Windows.UI;
using Windows.UI.ViewManagement;

namespace KYNXA_Desktop;

public partial class App
{
    private async Task CheckPopupAppearanceAsync(Window settings, FrameworkElement settingsRoot, bool dropdownsOnly = false)
    {
        string originalId = AppearanceService.Current.Id, originalMode = AppearanceService.Mode;
        double originalSize = AppearanceService.ChatFontSize;
        string? originalAccent = AppearanceService.CustomAccent;
        string draft = Prompt.Text;
        Guid? conversationId = ActiveId;
        string messages = JsonSerializer.Serialize(_shell.ActiveMessages.Select(row => row.Message));
        int writes = _gateway.Writes;
        var observations = new List<object>();
        var modePicker = NativeUi.ById<ComboBox>(settingsRoot, "AppearanceModePicker");
        var fontPicker = NativeUi.ById<ComboBox>(settingsRoot, "ChatFontSizePicker");
        MenuFlyout? menu = null;
        try
        {
            foreach (string mode in new[] { "light", "dark" })
            foreach (string accent in new[] { "#C02070", "#16836B" })
            {
                AppearanceService.ApplyPreferences(originalId, mode, originalSize, accent);
                await ActivatePopupSettingsAsync(settings);
                string label = mode + " " + accent;
                foreach (var picker in new[] { modePicker, fontPicker })
                {
                    picker.StartBringIntoView(new BringIntoViewOptions { AnimationDesired = false, VerticalAlignmentRatio = .2 });
                    await SettleAsync();
                    picker.IsDropDownOpen = true;
                    var selected = (ComboBoxItem)picker.SelectedItem;
                    await WaitAsync(() => selected.IsLoaded && VisualTreeHelper.GetOpenPopupsForXamlRoot(settingsRoot.XamlRoot)
                        .Any(popup => popup.IsOpen && popup.Child is not null && NativeUi.Descendants<ComboBoxItem>(popup.Child)
                            .Any(item => ReferenceEquals(item, selected))), label + " opens the actual production dropdown");
                    await SettleAsync();
                    var popup = VisualTreeHelper.GetOpenPopupsForXamlRoot(settingsRoot.XamlRoot).Single(candidate =>
                        candidate.IsOpen && candidate.Child is not null && NativeUi.Descendants<ComboBoxItem>(candidate.Child)
                            .Any(item => ReferenceEquals(item, selected)));
                    var popupBorder = NativeUi.ByName<Border>((FrameworkElement)popup.Child!, "PopupBorder");
                    Check(Math.Abs(popupBorder.ActualWidth - picker.ActualWidth) <= 2,
                        label + " keeps the native dropdown outer width aligned with its closed selection field");
                    Check(popupBorder.CornerRadius.Equals(new CornerRadius(16)),
                        label + " keeps all four corners of the actual dropdown popup rounded at sixteen DIPs");
                    var layout = NativeUi.ByName<Grid>(selected, "LayoutRoot");
                    if (ReferenceEquals(picker, modePicker))
                    {
                        var bounds = SidebarBounds(layout, popupBorder);
                        double rightInset = popupBorder.ActualWidth - bounds.Right;
                        Check(bounds.Left > 0 && bounds.Left <= 8 && rightInset > 0 && rightInset <= 8 &&
                            Math.Abs(bounds.Left - rightInset) < 1,
                            label + " short dropdown keeps narrow symmetric sides without an unused scrollbar gutter");
                    }
                    var common = VisualStateManager.GetVisualStateGroups(layout).Single(group => group.Name == "CommonStates");
                    string pickerLabel = label + " " + picker.GetType().Name + " " + picker.Name;
                    foreach (string state in new[] { "Selected", "SelectedUnfocused", "SelectedPointerOver", "SelectedPressed" })
                    {
                        // Exercise the SDK state on a loaded production item, without claiming physical presses.
                        // 在已加载的生产选项上核对 SDK 状态，不将视觉状态切换宣称为真实鼠标按压。
                        Check(VisualStateManager.GoToState(selected, state, false), pickerLabel + " exposes " + state);
                        await SettleAsync();
                        CheckSelectedPopupItem(selected, pickerLabel + " VSM " + state, observations);
                    }
                    MovePointerInsideSettings(settings, settingsRoot, localPoint: new Point(12, settingsRoot.ActualHeight - 12));
                    await SettleAsync();
                    bool entered = false;
                    PointerEventHandler onEntered = (_, _) => entered = true;
                    selected.PointerEntered += onEntered;
                    try
                    {
                        MovePointerInsideSettings(settings, selected, popup);
                        await WaitAsync(() => entered && common.CurrentState?.Name == "SelectedPointerOver",
                            pickerLabel + " receives real native selected-item hover");
                        CheckSelectedPopupItem(selected, pickerLabel + " real pointer hover", observations);
                        if (ReferenceEquals(picker, modePicker) && accent == "#C02070")
                            await RenderPopupElementAsync(popupBorder, "shell-popup-dropdown-" + mode + "-ui-render.png");
                    }
                    finally { selected.PointerEntered -= onEntered; }
                    picker.IsDropDownOpen = false;
                    await SettleAsync();
                }

                if (dropdownsOnly) continue;
                await ActivateOwnedShortcutWindowAsync();
                var fixtureChat = new ProjectChatState { Title = "POPUP_APPEARANCE_FIXTURE" };
                menu = (MenuFlyout)Call("CreateChatMenu", fixtureChat, null)!;
                await CheckMenuPopupAppearanceAsync(menu, Element<Button>("HistorySearchButton"), label + " production work menu", observations);
                menu.Hide();
                await SettleAsync();

                // The synthetic entries exercise global defaults that production menus create without explicit styles.
                // 独立条目验证生产动态菜单所依赖的全局默认样式，不触发保存或删除等业务动作。
                menu = new MenuFlyout();
                menu.Items.Add(new MenuFlyoutItem { Text = "Popup command" });
                menu.Items.Add(new ToggleMenuFlyoutItem { Text = "Popup checked command", IsChecked = true });
                var sub = new MenuFlyoutSubItem { Text = "Popup submenu" };
                sub.Items.Add(new MenuFlyoutItem { Text = "Popup nested command" });
                menu.Items.Add(sub);
                await CheckMenuPopupAppearanceAsync(menu, Element<Button>("HistorySearchButton"), label + " implicit menu defaults", observations);
                menu.Hide();
                await SettleAsync();
            }

            if (dropdownsOnly)
            {
                foreach (string mode in new[] { "light", "dark" })
                {
                    AppearanceService.ApplyPreferences(originalId, mode, originalSize, mode == "light" ? "#C02070" : "#16836B");
                    await CheckScrollableFontGutterAsync(settings, settingsRoot, mode, observations);
                }
            }
            else await CheckPickerPopupAppearanceAsync(settings, settingsRoot, observations);
            CheckHighContrastPopupResources();
            await ActivatePopupSettingsAsync(settings);
            fontPicker.StartBringIntoView(new BringIntoViewOptions { AnimationDesired = false });
            await SettleAsync();
            int originalIndex = fontPicker.SelectedIndex;
            fontPicker.Focus(FocusState.Keyboard);
            fontPicker.IsDropDownOpen = true;
            await SettleAsync();
            SendSettingsKey(settings, 0x28);
            SendSettingsKey(settings, 0x0D);
            await WaitAsync(() => !fontPicker.IsDropDownOpen && fontPicker.SelectedIndex == originalIndex + 1,
                "rounded dropdown preserves native Down and Enter selection");
            SelectAppearanceItem(fontPicker, originalSize);
            Check(Prompt.Text == draft && ActiveId == conversationId && _gateway.Writes == writes &&
                JsonSerializer.Serialize(_shell.ActiveMessages.Select(row => row.Message)) == messages,
                "popup appearance and keyboard checks preserve draft, active conversation and formal messages without gateway writes");
            File.WriteAllText(Path.Combine(_directory, "shell-popup-appearance.json"),
                JsonSerializer.Serialize(observations, new JsonSerializerOptions { WriteIndented = true }));
        }
        finally
        {
            menu?.Hide();
            modePicker.IsDropDownOpen = false;
            fontPicker.IsDropDownOpen = false;
            AppearanceService.ApplyPreferences(originalId, originalMode, originalSize, originalAccent);
            var layout = ShellField<LayoutState>("_layout");
            layout.AppearancePaletteId = originalId;
            layout.AppearanceMode = originalMode;
            layout.ChatFontSize = originalSize;
            layout.AppearanceAccent = originalAccent;
            new LayoutStateService().Save(layout);
            settings.Activate();
            await SettleAsync();
        }
    }

    private async Task ActivatePopupSettingsAsync(Window settings)
    {
        // A previously active main window can keep native focus after WinUI's asynchronous activation request.
        // WinUI 异步请求激活后，先前主窗口仍可能保有原生焦点；开始输入前显式核验测试设置窗口。
        await ActivateOwnedShortcutWindowAsync(settings);
    }

    private void CheckSelectedPopupItem(ComboBoxItem item, string label, List<object> observations)
    {
        var background = NativeUi.ByName<Grid>(item, "LayoutRoot");
        var content = NativeUi.ByName<ContentPresenter>(item, "ContentPresenter");
        var pill = NativeUi.ByName<Rectangle>(item, "Pill");
        var palette = AppearanceService.Current;
        Check(item.IsSelected && NativeUi.IsVisible(item) && IsEightDips(item.CornerRadius) && IsEightDips(background.CornerRadius) &&
            background.Margin.Left > 0 && background.Margin.Right > 0,
            label + " paints the actual selected background with eight-DIP corners and an inset from the popup edges");
        Check(background.Background is SolidColorBrush brush && content.Foreground is SolidColorBrush foreground &&
            brush.Color == AppearanceService.ParseColor(palette.IsHighContrast ? palette.Accent : palette.Soft) &&
            foreground.Color == AppearanceService.ParseColor(palette.IsHighContrast ? palette.OnAccent : palette.Text) &&
            PopupTextContrast(content.Foreground, background.Background, palette.Sidebar) >= 4.5,
            label + " uses the live application accent tint and readable foreground instead of OS blue");
        Check(pill.Opacity > .99 && pill.Fill is SolidColorBrush marker &&
            marker.Color == AppearanceService.ParseColor(palette.IsHighContrast ? palette.OnAccent : palette.AccentText),
            label + " retains the native selected indicator in the application accent");
        observations.Add(new { Label = label, Surface = "ComboBoxItem", Palette = palette.Id, palette.IsDark,
            CustomAccent = AppearanceService.CustomAccent, Background = PopupBrushHex(background.Background),
            Foreground = PopupBrushHex(content.Foreground), Marker = PopupBrushHex(pill.Fill), background.CornerRadius,
            background.Margin, background.ActualWidth, background.ActualHeight });
    }

    private async Task CheckMenuPopupAppearanceAsync(MenuFlyout menu, FrameworkElement anchor, string label, List<object> observations)
    {
        menu.ShowAt(anchor);
        var items = menu.Items.OfType<Control>().ToArray();
        await WaitAsync(() => items.All(item => item.IsLoaded && item.ActualWidth > 0), label + " realizes native menu rows");
        await SettleAsync();
        var presenter = VisualTreeHelper.GetOpenPopupsForXamlRoot(_shell.XamlRoot)
            .Where(popup => popup.IsOpen && popup.Child is not null)
            .SelectMany(popup => NativeUi.Descendants<MenuFlyoutPresenter>(popup.Child!))
            .Single(candidate => NativeUi.Descendants<Control>(candidate).Any(item => ReferenceEquals(item, items[0])));
        Check(presenter.CornerRadius.Equals(new CornerRadius(16)), label + " retains sixteen-DIP outer popup corners");
        foreach (var item in items)
        foreach (string state in item is MenuFlyoutSubItem
            ? new[] { "PointerOver", "Pressed", "SubMenuOpened" } : new[] { "PointerOver", "Pressed" })
        {
            Check(VisualStateManager.GoToState(item, state, false), label + " exposes " + item.GetType().Name + " " + state);
            await SettleAsync();
            var layout = NativeUi.ByName<Grid>(item, "LayoutRoot");
            var text = NativeUi.ByName<TextBlock>(item, "TextBlock");
            Check(IsEightDips(item.CornerRadius) && IsEightDips(layout.CornerRadius) && NativeUi.IsVisible(layout) &&
                layout.Background is SolidColorBrush brush && brush.Color.A > 0 &&
                PopupTextContrast(text.Foreground, layout.Background, AppearanceService.Current.Sidebar) >= 4.5,
                label + " " + item.GetType().Name + " " + state + " paints a rounded native highlight with readable text");
            if (item is MenuFlyoutSubItem)
            {
                var chevron = NativeUi.ByName<FontIcon>(item, "SubItemChevron");
                Check(NativeUi.IsVisible(chevron) &&
                    PopupTextContrast(chevron.Foreground, layout.Background, AppearanceService.Current.Sidebar) >= 3,
                    label + " " + state + " preserves a visible native submenu chevron against its actual highlight");
            }
            observations.Add(new { Label = label, Surface = item.GetType().Name, State = state, VisualStateOnly = true,
                Background = PopupBrushHex(layout.Background), Foreground = PopupBrushHex(text.Foreground), layout.CornerRadius,
                layout.ActualWidth, layout.ActualHeight });
            if (ReferenceEquals(item, items[0]) && state == "PointerOver" &&
                label.EndsWith("production work menu", StringComparison.Ordinal) && AppearanceService.CustomAccent == "#C02070")
                await RenderPopupElementAsync(presenter,
                    "shell-popup-menu-" + (AppearanceService.Current.IsDark ? "dark" : "light") + "-ui-render.png");
            VisualStateManager.GoToState(item, "Normal", false);
        }
    }

    private void CheckHighContrastPopupResources()
    {
        var colors = new Dictionary<UIElementType, Color>
        {
            [UIElementType.Window] = AppearanceService.ParseColor("#000000"),
            [UIElementType.WindowText] = AppearanceService.ParseColor("#FFFFFF"),
            [UIElementType.Highlight] = AppearanceService.ParseColor("#FFFF00"),
            [UIElementType.HighlightText] = AppearanceService.ParseColor("#000000"),
            [UIElementType.Hotlight] = AppearanceService.ParseColor("#00FFFF"),
            [UIElementType.ButtonFace] = AppearanceService.ParseColor("#000000"),
            [UIElementType.GrayText] = AppearanceService.ParseColor("#808080")
        };
        var palette = AppearanceService.ResolveHighContrastPalette(AppearanceService.Current, type => colors[type]);
        var apply = typeof(AppearanceService).GetMethod("ApplyPopupPalette", BindingFlags.Static | BindingFlags.NonPublic)!;
        try
        {
            // Resolve supplied OS colors without changing Windows accessibility settings or the active preference.
            // 使用给定系统色验证资源配对，不更改 Windows 辅助设置或当前外观偏好。
            apply.Invoke(null, [palette]);
            foreach (string state in new[] { "Selected", "SelectedUnfocused", "SelectedPointerOver", "SelectedPressed" })
                Check(AppearanceService.GetBrush("ComboBoxItemBackground" + state).Color == colors[UIElementType.Highlight] &&
                    AppearanceService.GetBrush("ComboBoxItemForeground" + state).Color == colors[UIElementType.HighlightText] &&
                    AppearanceService.GetBrush("ComboBoxItemBorderBrush" + state).Color == colors[UIElementType.Highlight] &&
                    AppearanceService.ContrastRatio(palette.Accent, palette.OnAccent) >= 4.5,
                    "supplied high-contrast colors keep the " + state + " popup highlight, border and text paired");
            Check(AppearanceService.GetBrush("ComboBoxItemPillFillBrush").Color == colors[UIElementType.HighlightText] &&
                AppearanceService.GetBrush("MenuFlyoutSubItemBackgroundSubMenuOpened").Color == colors[UIElementType.Highlight] &&
                AppearanceService.GetBrush("MenuFlyoutSubItemForegroundSubMenuOpened").Color == colors[UIElementType.HighlightText] &&
                AppearanceService.GetBrush("MenuFlyoutSubItemChevronSubMenuOpened").Color == colors[UIElementType.HighlightText],
                "supplied high-contrast colors pair the selected marker and opened submenu text and chevron with HighlightText");
        }
        finally { apply.Invoke(null, [AppearanceService.Current]); }
    }

    private async Task RenderPopupElementAsync(FrameworkElement element, string filename)
    {
        // Render this isolated native element; the PNG is not an operating-system screen capture.
        // 仅渲染隔离原生元素；此 PNG 不是操作系统屏幕截图。
        try
        {
            var image = new RenderTargetBitmap();
            await image.RenderAsync(element);
            using var pixels = DataReader.FromBuffer(await image.GetPixelsAsync());
            byte[] bgra = new byte[pixels.UnconsumedBufferLength]; pixels.ReadBytes(bgra);
            if (image.PixelWidth == 0 || image.PixelHeight == 0 || !bgra.Where((_, index) => index % 4 == 3).Any(alpha => alpha > 0))
                throw new NotSupportedException("WinUI returned no visible popup pixels.");
            using var stream = new InMemoryRandomAccessStream();
            var encoder = await BitmapEncoder.CreateAsync(BitmapEncoder.PngEncoderId, stream);
            encoder.SetPixelData(BitmapPixelFormat.Bgra8, BitmapAlphaMode.Premultiplied,
                (uint)image.PixelWidth, (uint)image.PixelHeight, 96, 96, bgra);
            await encoder.FlushAsync();
            stream.Seek(0);
            await using var output = File.Create(Path.Combine(_directory, filename));
            await stream.AsStreamForRead().CopyToAsync(output);
        }
        catch (Exception error) when (error is COMException or NotSupportedException or ArgumentException)
        {
            File.WriteAllText(Path.Combine(_directory, filename + ".unavailable.txt"), error.ToString());
        }
    }

    private static string? PopupBrushHex(Brush brush) => brush is SolidColorBrush solid ? ToHex(solid.Color) : null;

    private static double PopupTextContrast(Brush foreground, Brush background, string parentBackground)
    {
        if (foreground is not SolidColorBrush text || background is not SolidColorBrush fill) return 0;
        static Color Composite(Color color, Color parent)
        {
            double alpha = color.A / 255d;
            return Color.FromArgb(255, (byte)Math.Round(color.R * alpha + parent.R * (1 - alpha)),
                (byte)Math.Round(color.G * alpha + parent.G * (1 - alpha)),
                (byte)Math.Round(color.B * alpha + parent.B * (1 - alpha)));
        }
        var surface = Composite(fill.Color, AppearanceService.ParseColor(parentBackground));
        return AppearanceService.ContrastRatio(ToHex(Composite(text.Color, surface)), ToHex(surface));
    }
}

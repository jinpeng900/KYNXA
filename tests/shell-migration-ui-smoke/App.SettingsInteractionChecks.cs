using System.Runtime.InteropServices;
using System.Text;
using System.Text.Json;
using KYNXA_Desktop.Controls;
using KYNXA_Desktop.Services;
using MemoryUiSmoke;
using Microsoft.UI.Windowing;
using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Automation;
using Microsoft.UI.Xaml.Controls;
using Microsoft.UI.Xaml.Controls.Primitives;
using Microsoft.UI.Xaml.Input;
using Microsoft.UI.Xaml.Media;
using Windows.Foundation;

namespace KYNXA_Desktop;

public partial class App
{
    private void RequireSettingsForeground(Window window)
    {
        if (GetForegroundWindow() != WinRT.Interop.WindowNative.GetWindowHandle(window))
            throw new InvalidOperationException("Settings input stopped because the task-owned window is no longer foreground.");
    }

    private void MovePointerInsideSettings(Window window, FrameworkElement element, Popup? expectedPopup = null,
        Point? localPoint = null)
    {
        RequireSettingsForeground(window);
        var root = (FrameworkElement)window.Content;
        var point = element.TransformToVisual(root).TransformPoint(localPoint ??
            new Point(element.ActualWidth / 2, element.ActualHeight / 2));
        double scale = root.XamlRoot.RasterizationScale;
        var screenPoint = new NativePoint { X = (int)Math.Round(point.X * scale), Y = (int)Math.Round(point.Y * scale) };
        nint handle = WinRT.Interop.WindowNative.GetWindowHandle(window);
        if (!ClientToScreen(handle, ref screenPoint))
            throw new InvalidOperationException("The settings wheel target cannot be located on screen.");
        nint hitRoot = GetAncestor(WindowFromPoint(screenPoint), 2);
        bool knownPopup = IsOwnedSettingsPopup(handle, hitRoot, root, element, expectedPopup);
        File.WriteAllText(Path.Combine(_directory, "settings-popup-pointer.json"), JsonSerializer.Serialize(new
        {
            Target = element.GetType().Name, ExpectedPopup = expectedPopup is not null, point.X, point.Y,
            ScreenX = screenPoint.X, ScreenY = screenPoint.Y, Scale = scale, Handle = handle.ToInt64(),
            HitRoot = hitRoot.ToInt64(), HitClass = SettingsWindowClass(hitRoot), KnownPopup = knownPopup,
            Owner = GetSettingsNativeWindow(hitRoot, 4).ToInt64()
        }, new JsonSerializerOptions { WriteIndented = true }));
        if (hitRoot != handle && !knownPopup)
            throw new InvalidOperationException($"The settings wheel target is clipped or belongs to another window: " +
                $"point ({screenPoint.X},{screenPoint.Y}), class {SettingsWindowClass(hitRoot)}, root {hitRoot}, owner {GetSettingsNativeWindow(hitRoot, 4)}, expected {handle}.");
        int left = GetSystemMetrics(76), top = GetSystemMetrics(77);
        int width = GetSystemMetrics(78), height = GetSystemMetrics(79);
        if (width <= 1 || height <= 1 || screenPoint.X < left || screenPoint.X >= left + width ||
            screenPoint.Y < top || screenPoint.Y >= top + height)
            throw new InvalidOperationException("The settings wheel target is outside the virtual screen.");
        RequireSettingsForeground(window);
        SendFixtureInput([new NativeInput { Data = new NativeInputData { Mouse = new NativeMouseInput
        {
            X = (int)Math.Round((screenPoint.X - left) * 65535d / (width - 1)),
            Y = (int)Math.Round((screenPoint.Y - top) * 65535d / (height - 1)), Flags = 0x8000 | 0x4000 | 0x0001
        } } }]);
        if (!GetCursorPos(out var actual) || Math.Abs(actual.X - screenPoint.X) > 1 || Math.Abs(actual.Y - screenPoint.Y) > 1)
            throw new InvalidOperationException("The settings test pointer did not reach its verified target.");
    }

    private static bool IsOwnedSettingsPopup(nint owner, nint hitRoot, FrameworkElement root,
        FrameworkElement element, Popup? popup)
    {
        if (popup is not { IsOpen: true, Child: not null } || !ReferenceEquals(popup.XamlRoot, root.XamlRoot) ||
            !NativeUi.Descendants<DependencyObject>(popup.Child).Any(candidate => ReferenceEquals(candidate, element))) return false;
        GetSettingsWindowProcessId(hitRoot, out uint processId);
        if (processId != Environment.ProcessId) return false;
        for (nint current = hitRoot; current != 0; current = GetSettingsNativeWindow(current, 4))
        {
            if (current == owner) return true;
            nint next = GetSettingsNativeWindow(current, 4);
            if (next == current) break;
        }
        // WinUI can host popup content in a separate bridge HWND without a GW_OWNER chain.
        // The exact open XamlRoot popup and process are verified above; the target's PointerOver is checked before wheel input.
        // WinUI 弹出内容可使用没有 GW_OWNER 链的独立桥接 HWND；上方已核对确切的已打开弹出树与进程，
        // 发送滚轮前还必须确认目标选项真实进入 PointerOver，不能仅凭同进程就向其他窗口输入。
        return SettingsWindowClass(hitRoot).Contains("PopupWindowSiteBridge", StringComparison.Ordinal);
    }

    private static string SettingsWindowClass(nint handle)
    {
        var name = new StringBuilder(256);
        GetSettingsNativeClassName(handle, name, name.Capacity);
        return name.ToString();
    }

    [DllImport("user32.dll", EntryPoint = "GetWindow")] private static extern nint GetSettingsNativeWindow(nint handle, uint command);
    [DllImport("user32.dll", EntryPoint = "GetWindowThreadProcessId")] private static extern uint GetSettingsWindowProcessId(nint handle, out uint processId);
    [DllImport("user32.dll", EntryPoint = "GetClassNameW", CharSet = CharSet.Unicode)] private static extern int GetSettingsNativeClassName(nint handle, StringBuilder name, int capacity);

    private void SendSettingsWheel(Window window, int delta)
    {
        RequireSettingsForeground(window);
        SendFixtureInput([new NativeInput { Data = new NativeInputData { Mouse = new NativeMouseInput
            { MouseData = unchecked((uint)delta), Flags = 0x0800 } } }]);
    }

    private void SendSettingsKey(Window window, ushort virtualKey)
    {
        RequireSettingsForeground(window);
        foreach (int modifier in new[] { 0x10, 0x11, 0x12, 0x5B, 0x5C })
            if ((GetAsyncKeyState(modifier) & 0x8000) != 0)
                throw new InvalidOperationException("Settings key input stopped because a modifier is already held.");
        NativeInput Key(bool up) => new() { Type = 1,
            Data = new NativeInputData { Keyboard = new NativeKeyboardInput { VirtualKey = virtualKey, Flags = up ? 2u : 0u } } };
        SendFixtureInput([Key(false), Key(true)]);
    }

    private async Task CheckSettingsInteractionsAsync()
    {
        Window? settings = null;
        var observations = new List<object>();
        GetCursorPos(out var originalPointer);
        int initialWrites = _gateway.Writes;
        try
        {
            Call("StorageSettings_Click", _shell, new RoutedEventArgs());
            settings = ShellField<Window>("_storageSettingsWindow");
            var root = (FrameworkElement)settings.Content;
            await WaitAsync(() => root.IsLoaded && root.XamlRoot is not null, "the actual settings window loads before layout and native wheel checks");
            await SettleAsync();
            var scroll = NativeUi.ById<ScrollViewer>(root, "SettingsScrollViewer");
            var content = NativeUi.ById<StackPanel>(root, "SettingsContentColumn");
            var presenter = (OverlappedPresenter)settings.AppWindow.Presenter;
            double scale = root.XamlRoot.RasterizationScale;

            Rect ContentBounds() => content.TransformToVisual(scroll)
                .TransformBounds(new Rect(0, 0, content.ActualWidth, content.ActualHeight));
            void RecordGeometry(string label)
            {
                var bounds = ContentBounds();
                observations.Add(new { Label = label, Width = scroll.ActualWidth, Height = scroll.ActualHeight,
                    ContentWidth = content.ActualWidth, bounds.Left, bounds.Right, bounds.Top,
                    CenterError = Math.Abs(bounds.Left + bounds.Right - scroll.ActualWidth) / 2,
                    Viewport = scroll.ViewportWidth, ScrollableHeight = scroll.ScrollableHeight, Scale = scale });
            }
            void CheckFilledWidth(string label)
            {
                var bounds = ContentBounds();
                RecordGeometry(label);
                Check(bounds.Width > 200 && bounds.Left >= 23 && bounds.Right <= scroll.ActualWidth - 23 &&
                    Math.Abs(bounds.Left + bounds.Right - scroll.ActualWidth) <= 2,
                    label + " centers the complete settings column with balanced margins and no horizontal clipping");
                Check(scroll.ActualWidth >= root.ActualWidth - 1 &&
                    Math.Abs(content.ActualWidth - (scroll.ActualWidth - 48)) <= 2,
                    label + " fills the available window width with only the two 24 DIP margins");
            }
            async Task ResizeAsync(double width, double height)
            {
                presenter.Restore();
                settings.AppWindow.Resize(new Windows.Graphics.SizeInt32((int)Math.Ceiling(width * scale), (int)Math.Ceiling(height * scale)));
                settings.Activate();
                await SettleAsync();
                root.UpdateLayout();
                scroll.ChangeView(null, 0, null, true);
                await SettleAsync();
            }

            await ResizeAsync(660, 560);
            CheckFilledWidth("restored 660 DIP settings window");
            await NativeWindowCapture.CaptureAsync(settings, Path.Combine(_directory, "settings-filled-restored.png"));
            presenter.Maximize();
            await SettleAsync();
            root.UpdateLayout();
            Check(presenter.State == OverlappedPresenterState.Maximized, "the real native settings window is maximized for the reported layout case");
            CheckFilledWidth("maximized settings window");
            await NativeWindowCapture.CaptureAsync(settings, Path.Combine(_directory, "settings-filled-maximized.png"));

            // Compare the previous centered, capped layout inside the owned tree, then restore the product values.
            // 仅在自有界面中对照上一版居中限宽布局，再恢复产品值；不回滚源码或用户设置。
            var filledAlignment = content.HorizontalAlignment;
            double filledMaximumWidth = content.MaxWidth;
            content.HorizontalAlignment = HorizontalAlignment.Center;
            content.MaxWidth = 900;
            root.UpdateLayout();
            await SettleAsync();
            RecordGeometry("controlled previous capped layout reproduction");
            await NativeWindowCapture.CaptureAsync(settings, Path.Combine(_directory, "settings-capped-layout-reproduction.png"));
            var originalBounds = ContentBounds();
            Check(scroll.ActualWidth > 950 && originalBounds.Width < scroll.ActualWidth - 50,
                "the previous centered width cap leaves unused space in the actual maximized window");
            content.HorizontalAlignment = filledAlignment;
            content.MaxWidth = filledMaximumWidth;
            root.UpdateLayout();
            await SettleAsync();
            CheckFilledWidth("maximized settings after restoring the responsive layout");

            await ResizeAsync(1000, 680);
            CheckFilledWidth("restored 1000 DIP settings window after maximization");
            await NativeWindowCapture.CaptureAsync(settings, Path.Combine(_directory, "settings-filled-wide.png"));

            await ResizeAsync(420, 560);
            CheckFilledWidth("narrow 420 DIP settings window");
            var paletteGrid = NativeUi.ById<Grid>(root, "AppearancePaletteGrid");
            var paletteBounds = paletteGrid.TransformToVisual(scroll).TransformBounds(new Rect(0, 0, paletteGrid.ActualWidth, paletteGrid.ActualHeight));
            Check(paletteBounds.Left >= 0 && paletteBounds.Right <= scroll.ActualWidth + 1 && scroll.ScrollableHeight > 100,
                "narrow settings keep the palette grid inside the viewport and all lower sections vertically reachable");
            await NativeWindowCapture.CaptureAsync(settings, Path.Combine(_directory, "settings-filled-narrow.png"));
            await ResizeAsync(360, 560);
            CheckFilledWidth("minimum 360 DIP settings window");
            await NativeWindowCapture.CaptureAsync(settings, Path.Combine(_directory, "settings-filled-minimum.png"));
            string layoutLanguage = UiText.Language;
            try
            {
                foreach (string languageId in new[] { "zh-CN", "en" })
                {
                    UiText.Initialize(languageId);
                    await SettleAsync();
                    CheckFilledWidth(languageId + " minimum-width settings window");
                    var narrowControls = new[] { "InterfaceLanguagePicker", "AppearanceModePicker", "ChatFontSizePicker",
                        "AppearanceAccentInput", "AppearanceColorPickerButton", "AppearanceAccentApplyButton", "AppearanceAccentResetButton" }
                        .Select(id => NativeUi.ById<FrameworkElement>(root, id)).ToArray();
                    Check(narrowControls.All(control =>
                    {
                        var bounds = control.TransformToVisual(scroll).TransformBounds(new Rect(0, 0, control.ActualWidth, control.ActualHeight));
                        return bounds.Width > 0 && bounds.Left >= 23 && bounds.Right <= scroll.ActualWidth - 23;
                    }) && scroll.ScrollableWidth <= 1,
                        languageId + " minimum-width settings keep dropdowns and accent actions inside the viewport without horizontal scrolling");
                    Check(paletteGrid.ColumnDefinitions.Count == 2 && NativeUi.Descendants<Button>(paletteGrid).Count(button =>
                        AutomationProperties.GetAutomationId(button).StartsWith("AppearancePalette_", StringComparison.Ordinal)) == 5,
                        languageId + " minimum-width settings retain all five palette cards in the responsive two-column grid");
                    scroll.ChangeView(null, scroll.ScrollableHeight, null, true);
                    await SettleAsync();
                    Check(Math.Abs(scroll.VerticalOffset - scroll.ScrollableHeight) <= 1,
                        languageId + " minimum-width settings can reach the bottom of the complete form");
                    await NativeWindowCapture.CaptureAsync(settings, Path.Combine(_directory, "settings-filled-minimum-bottom-" + languageId + ".png"));
                }
            }
            finally { UiText.Initialize(layoutLanguage); }
            await ResizeAsync(660, 560);

            var mode = NativeUi.ById<ComboBox>(root, "AppearanceModePicker");
            var font = NativeUi.ById<ComboBox>(root, "ChatFontSizePicker");
            var language = NativeUi.ById<ComboBox>(root, "InterfaceLanguagePicker");
            Check(new[] { mode, font, language }.All(picker => picker is ScrollSafeComboBox),
                "all actual settings dropdowns use the shared native wheel-safe control");

            // Compare framework and guarded controls with real wheel input in the same production scroll host.
            // 在同一生产滚动宿主中对比框架控件与保护控件，使用真实滚轮输入而非手动触发事件。
            var plain = new ComboBox { HorizontalAlignment = HorizontalAlignment.Stretch };
            plain.Items.Add("BASELINE_0"); plain.Items.Add("BASELINE_1"); plain.Items.Add("BASELINE_2");
            plain.SelectedIndex = 1;
            content.Children.Insert(0, plain);
            root.UpdateLayout();
            scroll.ChangeView(null, 0, null, true);
            settings.Activate();
            plain.Focus(FocusState.Programmatic);
            await SettleAsync();
            MovePointerInsideSettings(settings, plain);
            SendSettingsWheel(settings, -120);
            await SettleAsync();
            observations.Add(new { Label = "native plain ComboBox controlled wheel reproduction", SelectedBefore = 1,
                SelectedAfter = plain.SelectedIndex, ScrollOffset = scroll.VerticalOffset });
            Check(plain.SelectedIndex != 1, "a focused native plain ComboBox reproduces the original accidental wheel selection");
            content.Children.Remove(plain);

            var guarded = new ScrollSafeComboBox { HorizontalAlignment = HorizontalAlignment.Stretch, MaxDropDownHeight = 180 };
            foreach (int index in Enumerable.Range(0, 40)) guarded.Items.Add("WHEEL_FIXTURE_" + index.ToString("00"));
            guarded.SelectedIndex = 1;
            content.Children.Insert(0, guarded);
            try
            {
                root.UpdateLayout();
                scroll.ChangeView(null, 0, null, true);
                guarded.Focus(FocusState.Programmatic);
                await SettleAsync();
                MovePointerInsideSettings(settings, guarded);
                double before = scroll.VerticalOffset;
                SendSettingsWheel(settings, -120);
                await WaitAsync(() => scroll.VerticalOffset > before + 1, "wheel over a focused closed guarded dropdown scrolls its parent page");
                Check(guarded.SelectedIndex == 1 && !guarded.IsDropDownOpen,
                    "the guarded closed dropdown retains selection while its parent actually scrolls");

                scroll.ChangeView(null, 0, null, true);
                await SettleAsync();
                guarded.Focus(FocusState.Keyboard);
                SendSettingsKey(settings, 0x28);
                await WaitAsync(() => guarded.SelectedIndex == 2, "native Down arrow still changes the focused closed dropdown choice");
                Check(guarded.SelectedIndex == 2, "wheel protection preserves explicit native keyboard selection");
                SendSettingsKey(settings, 0x24);
                await WaitAsync(() => guarded.SelectedIndex == 0, "native Home still selects the first dropdown item");
                guarded.IsDropDownOpen = true;
                await WaitAsync(() => VisualTreeHelper.GetOpenPopupsForXamlRoot(root.XamlRoot).Any(popup => popup.IsOpen && popup.Child is not null &&
                    NativeUi.Descendants<ComboBoxItem>(popup.Child).Any(item => item.Content?.ToString()?.StartsWith("WHEEL_FIXTURE_", StringComparison.Ordinal) == true)),
                    "the guarded dropdown opens its real native popup");
                await SettleAsync();
                var popup = VisualTreeHelper.GetOpenPopupsForXamlRoot(root.XamlRoot).Single(candidate => candidate.IsOpen && candidate.Child is not null &&
                    NativeUi.Descendants<ComboBoxItem>(candidate.Child).Any(item => item.Content?.ToString()?.StartsWith("WHEEL_FIXTURE_", StringComparison.Ordinal) == true));
                var optionsScroll = NativeUi.Descendants<ScrollViewer>(popup.Child!).First(viewer => viewer.ScrollableHeight > 0);
                var visibleOption = NativeUi.Descendants<ComboBoxItem>(popup.Child!).First(item =>
                {
                    if (item.ActualWidth <= 0 || item.ActualHeight <= 0) return false;
                    var bounds = item.TransformToVisual(optionsScroll).TransformBounds(new Rect(0, 0, item.ActualWidth, item.ActualHeight));
                    return bounds.Top >= 0 && bounds.Bottom <= optionsScroll.ActualHeight;
                });
                bool pointerOverOption = false;
                PointerEventHandler optionEntered = (_, _) => pointerOverOption = true;
                PointerEventHandler optionExited = (_, _) => pointerOverOption = false;
                visibleOption.PointerEntered += optionEntered;
                visibleOption.PointerExited += optionExited;
                try
                {
                    // The option popup can cover the title bar; leave it through the page's lower left margin.
                    // 下拉列表可能遮住标题栏，先移到页面左下方留白处，确保下一次进入选项产生真实事件。
                    MovePointerInsideSettings(settings, root, localPoint: new Point(12, root.ActualHeight - 12));
                    await SettleAsync();
                    MovePointerInsideSettings(settings, visibleOption, popup);
                    await WaitAsync(() => pointerOverOption,
                        "the native pointer enters the exact option in the task-owned open popup before wheel input");
                    double popupBefore = optionsScroll.VerticalOffset;
                    SendSettingsWheel(settings, -120);
                    await WaitAsync(() => optionsScroll.VerticalOffset > popupBefore + 1, "wheel inside the open native dropdown scrolls its option list");
                }
                finally
                {
                    visibleOption.PointerEntered -= optionEntered;
                    visibleOption.PointerExited -= optionExited;
                }
                Check(guarded.IsDropDownOpen, "scrolling the open dropdown keeps its native option popup open");
                guarded.IsDropDownOpen = false;
                await SettleAsync();
            }
            finally
            {
                guarded.IsDropDownOpen = false;
                content.Children.Remove(guarded);
            }

            foreach (var picker in new[] { language, mode, font })
            {
                picker.StartBringIntoView(new BringIntoViewOptions { AnimationDesired = false, VerticalAlignmentRatio = 0.15 });
                picker.Focus(FocusState.Programmatic);
                settings.Activate();
                await SettleAsync();
                int selected = picker.SelectedIndex;
                string stored = File.ReadAllText(Path.Combine(DesktopDirectory, "layout.json"));
                MovePointerInsideSettings(settings, picker);
                SendSettingsWheel(settings, -120);
                await SettleAsync();
                Check(picker.SelectedIndex == selected && File.ReadAllText(Path.Combine(DesktopDirectory, "layout.json")) == stored,
                    AutomationProperties.GetAutomationId(picker) + " ignores closed-control wheel selection without writing a preference");
            }
            Check(_gateway.Writes == initialWrites, "settings layout and dropdown checks never write formal gateway data");
            Check(_unhandled is null, "settings resize, focus, native keyboard and wheel checks complete without an unhandled UI exception");
        }
        finally
        {
            File.WriteAllText(Path.Combine(_directory, "settings-layout-wheel-observations.json"),
                JsonSerializer.Serialize(observations, new JsonSerializerOptions { WriteIndented = true }));
            settings?.Close();
            Window.Activate();
            if (GetForegroundWindow() == WinRT.Interop.WindowNative.GetWindowHandle(Window))
                SetCursorPos(originalPointer.X, originalPointer.Y);
        }
    }
}

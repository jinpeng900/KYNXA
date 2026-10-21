using System.Diagnostics;
using System.Reflection;
using System.Runtime.InteropServices;
using System.Text.Json;
using KYNXA_Desktop.Controls;
using KYNXA_Desktop.Layout;
using KYNXA_Desktop.Models.UI;
using KYNXA_Desktop.Services;
using MemoryUiSmoke;
using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Automation;
using Microsoft.UI.Xaml.Controls;
using Microsoft.UI.Xaml.Input;
using Microsoft.UI.Xaml.Media;
using Windows.Foundation;

namespace KYNXA_Desktop;

public partial class App
{
    private async Task CheckSidebarControlsAsync()
    {
        if (ActiveId is null) await SearchAndSelectAsync(_gateway.WorkChatId, "Work fixture");
        // Wait for the legitimate latest-history read before taking the immutable row snapshot used by all sidebar checks.
        // 所有侧栏检查共用的原始行快照，必须在合法的最近历史读取完成之后取得。
        await WaitAsync(() =>
        {
            if (_gateway.Failure is not null) throw new InvalidOperationException("Sidebar fixture history failed before native control checks.", _gateway.Failure);
            var histories = ShellField<System.Collections.IDictionary>("_historyPages");
            object? state = ActiveId is Guid chatId ? histories[chatId] : null;
            return state is not null && (bool)state.GetType().GetProperty("Initialized")!.GetValue(state)! &&
                !(bool)state.GetType().GetProperty("Loading")!.GetValue(state)!;
        }, "the selected fixture history settles before native sidebar snapshots and pointer input");
        var layout = ShellField<LayoutState>("_layout");
        double originalWidth = layout.SidebarWidth;
        bool originallyCollapsed = layout.SidebarCollapsed;
        string originalLanguage = UiText.Language;
        string originalPalette = AppearanceService.Current.Id;
        string originalMode = AppearanceService.Mode;
        double originalFontSize = AppearanceService.ChatFontSize;
        string? originalAccent = AppearanceService.CustomAccent;
        string originalDraft = Prompt.Text;
        Guid? originalChat = ActiveId;
        var originalRows = _shell.ActiveMessages.ToArray();
        string originalMessages = JsonSerializer.Serialize(originalRows.Select(row => row.Message).ToArray());
        var originalSize = Window.AppWindow.Size;
        var originalPosition = Window.AppWindow.Position;
        nint ownedWindow = WinRT.Interop.WindowNative.GetWindowHandle(Window);
        if (!GetCursorPos(out NativePoint originalPointer)) throw new InvalidOperationException("Cannot save the sidebar fixture pointer position.");
        const string draft = "SIDEBAR_CONTROLS_UNSENT_DRAFT 中文 $x^2$";
        var observations = new Dictionary<string, object>();
        var pane = Element<Grid>("SidebarPane");
        var expand = Element<Button>("CompactSidebarButton");
        var collapse = Element<Button>("CollapseSidebarButton");
        var scrim = Element<Button>("SidebarScrim");
        bool SameSavedSidebar(double width, bool collapsed)
        {
            var saved = new LayoutStateService().Load();
            return Math.Abs(saved.SidebarWidth - width) < 0.1 && saved.SidebarCollapsed == collapsed;
        }
        void CheckContext(string description) => Check(Prompt.Text == draft && ActiveId == originalChat &&
            _shell.ActiveMessages.SequenceEqual(originalRows) &&
            JsonSerializer.Serialize(_shell.ActiveMessages.Select(row => row.Message).ToArray()) == originalMessages &&
            _gateway.Writes == 0 && _gateway.Catalog.Revision == 7, description);
        try
        {
            NativeUi.SetText(Prompt, draft);
            ResizeInDips(1440, 900);
            await WaitAsync(() => Element<Grid>("ShellGrid").ActualWidth >= ShellLayoutMetrics.CompactSidebarBreakpoint,
                "sidebar control checks begin in the actual wide native layout");
            await ActivateOwnedShortcutWindowAsync();
            foreach (string language in new[] { "zh-CN", "en" })
            {
                UiText.Initialize(language);
                foreach (double width in new[] { ShellLayoutMetrics.SidebarDefault, ShellLayoutMetrics.SidebarMin, ShellLayoutMetrics.SidebarMax })
                {
                    layout.SidebarWidth = width;
                    layout.SidebarCollapsed = false;
                    Call("ApplyLayout");
                    await SettleAsync();
                    string key = language + "-" + width;
                    CheckSidebarHeaderGeometry(key, width, observations);
                    await CheckSidebarGripOverlapHitTestsAsync(key, observations);
                    if (language == "zh-CN" && width == ShellLayoutMetrics.SidebarDefault)
                        await CapturePresentedAsync("shell-sidebar-controls-wide.png");
                    if (language == "en" && width == ShellLayoutMetrics.SidebarMin)
                        await CapturePresentedAsync("shell-sidebar-controls-minimum-200-en.png");
                }
            }
            CheckContext("localized default, minimum and maximum header layouts preserve draft, active messages and the formal catalog");
            await CheckSidebarNavigationFeedbackAsync(observations);
            CheckContext("native navigation hover, pressed feedback and themed keyboard focus preserve draft, messages and the formal catalog");

            UiText.Initialize("zh-CN");
            layout.SidebarWidth = 318;
            layout.SidebarCollapsed = false;
            Call("ApplyLayout");
            Call("SaveLayout");
            await SettleAsync();
            Check(SameSavedSidebar(318, false), "the wide sidebar fixture saves its deliberate user width before native toggle checks");
            collapse.Focus(FocusState.Keyboard);
            NativeUi.Invoke(collapse);
            await WaitAsync(() => NativeUi.IsVisible(expand) && !NativeUi.IsVisible(collapse) &&
                Math.Abs(pane.ActualWidth - ShellLayoutMetrics.SidebarCollapsed) < 1,
                "the real new collapse button exposes its visible expand counterpart after native layout");
            Check(ReferenceEquals(FocusManager.GetFocusedElement(_shell.XamlRoot), expand) && expand.FocusState == FocusState.Keyboard,
                "wide native collapse preserves keyboard focus and its focus ring on the visible expand button");
            Check(SameSavedSidebar(318, true) && Math.Abs(pane.ActualWidth - ShellLayoutMetrics.SidebarCollapsed) < 1,
                "wide collapse persists the collapsed preference while retaining the expanded user width");
            CheckCollapsedSidebarGeometry("wide-collapsed", observations);
            await CapturePresentedAsync("shell-sidebar-controls-closed.png");
            expand.Focus(FocusState.Keyboard);
            NativeUi.Invoke(expand);
            await WaitAsync(() => NativeUi.IsVisible(collapse) && !NativeUi.IsVisible(expand) && Math.Abs(pane.ActualWidth - 318) < 1,
                "the real expand button restores the new collapse counterpart after native layout");
            Check(ReferenceEquals(FocusManager.GetFocusedElement(_shell.XamlRoot), collapse) && collapse.FocusState == FocusState.Keyboard,
                "wide native expansion preserves keyboard focus and its focus ring on the visible collapse button");
            Check(SameSavedSidebar(318, false) && Math.Abs(pane.ActualWidth - 318) < 1,
                "wide expansion saves the open preference and restores the previous user width");
            AppearanceService.ApplyPreferences(originalPalette, "dark", originalFontSize, originalAccent);
            await SettleAsync();
            CheckSidebarHeaderGeometry("wide-dark-318", 318, observations);
            await CheckSidebarGripOverlapHitTestsAsync("wide-dark-318", observations);
            await CapturePresentedAsync("shell-sidebar-controls-wide-dark.png");
            NativeUi.Invoke(collapse);
            await WaitAsync(() => NativeUi.IsVisible(expand), "the themed native collapse button closes the dark sidebar");
            await SettleAsync();
            CheckCollapsedSidebarGeometry("wide-collapsed-dark", observations);
            await CapturePresentedAsync("shell-sidebar-controls-closed-dark.png");
            NativeUi.Invoke(expand);
            await WaitAsync(() => NativeUi.IsVisible(collapse), "the dark rail restores its expanded sidebar");
            AppearanceService.ApplyPreferences(originalPalette, originalMode, originalFontSize, originalAccent);
            await SettleAsync();

            // Exercise the real grip's subscribed Shell handlers without manufacturing pointer events.
            // 触发真实拖动控件已订阅的 Shell 处理函数，不伪造指针事件。
            var grip = Element<ResizeGrip>("SidebarGrip");
            var hitTarget = NativeUi.ByName<Grid>(grip, "HitTarget");
            var indicator = NativeUi.ByName<Border>(grip, "Indicator");
            Check(grip.IndicatorBrush is SolidColorBrush transparent && transparent.Color.A == 0,
                "the sidebar grip explicitly uses a transparent indicator rather than the divider fallback");
            Check(NativeUi.IsVisible(grip) && grip.IsHitTestVisible && hitTarget.IsHitTestVisible && hitTarget.Background is not null &&
                Math.Abs(grip.ActualWidth - 8) < 0.1 && grip.ActualHeight > 50,
                "the transparent sidebar grip keeps its actual eight-DIP interactive hit target");
            var gripType = typeof(ResizeGrip);
            gripType.GetField("_pointerOver", PrivateInstance)!.SetValue(grip, true);
            gripType.GetMethod("UpdateIndicator", PrivateInstance)!.Invoke(grip, null);
            Check(indicator.Background is SolidColorBrush hovered && hovered.Color.A == 0,
                "the real grip indicator stays transparent in its hovered state");
            gripType.GetField("_pointerOver", PrivateInstance)!.SetValue(grip, false);
            gripType.GetMethod("UpdateIndicator", PrivateInstance)!.Invoke(grip, null);
            RaiseSidebarGripEvent(grip, "DragStarted", EventArgs.Empty);
            RaiseSidebarGripEvent(grip, "DragDelta", new ResizeDeltaEventArgs(-30));
            RaiseSidebarGripEvent(grip, "DragCompleted", EventArgs.Empty);
            await SettleAsync();
            Check(Math.Abs(pane.ActualWidth - 288) < 1 && SameSavedSidebar(288, false),
                "the transparent grip's real drag and completion subscriptions still resize and save the sidebar");
            RaiseSidebarGripEvent(grip, "DragStarted", EventArgs.Empty);
            RaiseSidebarGripEvent(grip, "DragDelta", new ResizeDeltaEventArgs(25));
            RaiseSidebarGripEvent(grip, "CancelRequested", EventArgs.Empty);
            await SettleAsync();
            Check(Math.Abs(pane.ActualWidth - 288) < 1 && SameSavedSidebar(288, false),
                "the original grip cancellation subscription restores the pre-drag width");
            RaiseSidebarGripEvent(grip, "ResetRequested", EventArgs.Empty);
            await SettleAsync();
            Check(Math.Abs(pane.ActualWidth - ShellLayoutMetrics.SidebarDefault) < 1 && SameSavedSidebar(ShellLayoutMetrics.SidebarDefault, false),
                "the original grip reset subscription still restores and saves the default width");

            layout.SidebarWidth = 318;
            Call("ApplyLayout");
            Call("SaveLayout");
            ResizeInDips(800, 720);
            await WaitAsync(() => Element<Grid>("ShellGrid").ActualWidth is > 0 and < ShellLayoutMetrics.CompactSidebarBreakpoint &&
                NativeUi.IsVisible(expand) && !NativeUi.IsVisible(collapse), "narrowing shows only the compact sidebar affordance");
            await SettleAsync();
            Check(!NativeUi.IsVisible(scrim) && !NativeUi.IsVisible(grip) && SameSavedSidebar(318, false),
                "entering compact mode closes the temporary drawer and preserves wide sidebar preferences");
            CheckCollapsedSidebarGeometry("compact-collapsed", observations);
            NativeUi.Invoke(expand);
            await WaitAsync(() => NativeUi.IsVisible(collapse) && NativeUi.IsVisible(scrim) && !NativeUi.IsVisible(expand) &&
                Math.Abs(pane.ActualWidth - 318) < 1,
                "the real compact button opens a temporary drawer with its new close button");
            Check(ReferenceEquals(FocusManager.GetFocusedElement(_shell.XamlRoot), collapse) && SameSavedSidebar(318, false),
                "opening the compact drawer focuses its visible close button without saving temporary state");
            CheckSidebarHeaderGeometry("compact-zh-CN-318", 318, observations);
            await CapturePresentedAsync("shell-sidebar-controls-compact.png");
            NativeUi.Invoke(collapse);
            await WaitAsync(() => NativeUi.IsVisible(expand) && !NativeUi.IsVisible(scrim) && !NativeUi.IsVisible(collapse),
                "the new close button dismisses the compact drawer");
            Check(ReferenceEquals(FocusManager.GetFocusedElement(_shell.XamlRoot), expand) && SameSavedSidebar(318, false),
                "the compact close button returns focus and leaves saved width and collapse preference untouched");

            NativeUi.Invoke(expand);
            await WaitAsync(() => NativeUi.IsVisible(scrim), "the compact drawer reopens before the original scrim dismissal");
            NativeUi.Invoke(scrim);
            await WaitAsync(() => !NativeUi.IsVisible(scrim) && NativeUi.IsVisible(expand), "the original native scrim still dismisses the drawer");
            Check(ReferenceEquals(FocusManager.GetFocusedElement(_shell.XamlRoot), expand) && SameSavedSidebar(318, false),
                "scrim dismissal returns focus without overwriting wide sidebar preferences");
            NativeUi.Invoke(expand);
            await WaitAsync(() => NativeUi.IsVisible(scrim), "the compact drawer reopens before the Escape dismissal path");
            Check((bool)Call("TryDismissCompactSidebar")! && !NativeUi.IsVisible(scrim) && NativeUi.IsVisible(expand) &&
                ReferenceEquals(FocusManager.GetFocusedElement(_shell.XamlRoot), expand) && SameSavedSidebar(318, false),
                "the production Escape dismissal path closes the drawer, returns focus and preserves saved preferences");
            ResizeInDips(1440, 900);
            await WaitAsync(() => Element<Grid>("ShellGrid").ActualWidth >= ShellLayoutMetrics.CompactSidebarBreakpoint &&
                NativeUi.IsVisible(collapse) && !NativeUi.IsVisible(expand), "restoring wide mode restores the expanded user's sidebar");
            await SettleAsync();
            Check(Math.Abs(pane.ActualWidth - 318) < 1 && !NativeUi.IsVisible(scrim) && SameSavedSidebar(318, false),
                "wide mode recovers the exact saved width and expanded preference after all temporary dismissals");

            NativeUi.Invoke(collapse);
            await WaitAsync(() => NativeUi.IsVisible(expand) && SameSavedSidebar(318, true), "the wide collapsed preference is saved before a second compact round trip");
            ResizeInDips(800, 720);
            await WaitAsync(() => Element<Grid>("ShellGrid").ActualWidth is > 0 and < ShellLayoutMetrics.CompactSidebarBreakpoint,
                "the second round trip reaches a real compact layout");
            NativeUi.Invoke(expand);
            await WaitAsync(() => NativeUi.IsVisible(collapse) && NativeUi.IsVisible(scrim), "a saved collapsed wide sidebar can still open its compact drawer");
            NativeUi.Invoke(collapse);
            await WaitAsync(() => NativeUi.IsVisible(expand) && !NativeUi.IsVisible(scrim), "the new button closes a drawer opened from a saved collapsed preference");
            Check(SameSavedSidebar(318, true), "a temporary drawer also preserves a previously collapsed wide preference");
            ResizeInDips(1440, 900);
            await WaitAsync(() => Element<Grid>("ShellGrid").ActualWidth >= ShellLayoutMetrics.CompactSidebarBreakpoint && NativeUi.IsVisible(expand),
                "returning wide respects the saved collapsed preference");
            await SettleAsync();
            Check(!NativeUi.IsVisible(collapse) && !NativeUi.IsVisible(scrim) &&
                Math.Abs(pane.ActualWidth - ShellLayoutMetrics.SidebarCollapsed) < 1 && SameSavedSidebar(318, true),
                "wide collapsed state survives compact opening and closing without a preference rewrite");
            CheckCollapsedSidebarGeometry("wide-collapsed-restored", observations);
            CheckContext("sidebar buttons, grip handlers, localization and compact dismissals preserve the exact draft, messages and zero-write catalog");
            File.WriteAllText(Path.Combine(_directory, "shell-sidebar-control-geometry.json"),
                JsonSerializer.Serialize(observations, new JsonSerializerOptions { WriteIndented = true }));
        }
        finally
        {
            // Restore only fixture-owned UI preferences and its original draft; no formal messages are edited.
            // 仅恢复夹具自己的 UI 偏好和原草稿，不编辑正式消息。
            UiText.Initialize(originalLanguage);
            AppearanceService.ApplyPreferences(originalPalette, originalMode, originalFontSize, originalAccent);
            layout.SidebarWidth = originalWidth;
            layout.SidebarCollapsed = originallyCollapsed;
            SetShellField("_temporarySidebarOpen", false);
            Window.AppWindow.MoveAndResize(new Windows.Graphics.RectInt32(originalPosition.X, originalPosition.Y,
                originalSize.Width, originalSize.Height));
            Call("ApplyLayout");
            Call("SaveLayout");
            NativeUi.SetText(Prompt, originalDraft);
            if (GetForegroundWindow() == ownedWindow) SetCursorPos(originalPointer.X, originalPointer.Y);
            await SettleAsync();
        }
    }

    private async Task CheckSidebarNavigationFeedbackAsync(Dictionary<string, object> observations)
    {
        string palette = AppearanceService.Current.Id;
        string mode = AppearanceService.Mode;
        double fontSize = AppearanceService.ChatFontSize;
        string? accent = AppearanceService.CustomAccent;
        var originalPosition = Window.AppWindow.Position;
        var originalSize = Window.AppWindow.Size;
        nint windowHandle = WinRT.Interop.WindowNative.GetWindowHandle(Window);
        if (!GetCursorPos(out NativePoint originalPointer)) throw new InvalidOperationException("Cannot read the original navigation pointer position.");
        var style = (Style)Current.Resources["KynxaNavigationButtonStyle"];
        var buttons = NativeUi.Descendants<Button>(Element<Grid>("SidebarPane"))
            .Where(button => ReferenceEquals(button.Style, style)).ToArray();
        var selection = AppearanceService.GetBrush("KynxaSelectionBrush");
        var focus = AppearanceService.GetBrush("KynxaFocusBrush");
        ContentPresenter? lastHovered = null;
        var pixelTraces = new List<SidebarNavigationPixelTrace>();
        Check(buttons.Length == 5, "all five actual sidebar navigation entries use the shared navigation style");
        var themes = new[]
        {
            (Key: "dark", Palette: "violet", Mode: "dark", Accent: (string?)null),
            (Key: "light", Palette: AppearanceService.DefaultPaletteId, Mode: "light", Accent: (string?)null),
            (Key: "custom-dark", Palette: "teal", Mode: "dark", Accent: (string?)"#C02070"),
            (Key: "custom-light", Palette: "teal", Mode: "light", Accent: (string?)"#C02070")
        };
        try
        {
            await ActivateOwnedShortcutWindowAsync();
            foreach (var theme in themes)
            {
                // Keep the last hovered entry under the pointer while changing the live palette.
                // 切换实时配色时保留上次入口的指针位置，核对已悬停背景也能立即更新。
                AppearanceService.ApplyPreferences(theme.Palette, theme.Mode, fontSize, theme.Accent);
                await SettleAsync();
                Check(ReferenceEquals(selection, AppearanceService.GetBrush("KynxaSelectionBrush")) &&
                    ReferenceEquals(Element<Border>("PrimaryModeSelectionPill").Background, selection) &&
                    selection.Color == AppearanceService.ParseColor(AppearanceService.Current.Soft),
                    theme.Key + " keeps navigation feedback and the mode selection on the same live palette brush");
                if (lastHovered is not null)
                    Check(ReferenceEquals(lastHovered.Background, selection),
                        theme.Key + " updates the already hovered navigation surface without recreating its brush");
                foreach (var button in buttons)
                {
                    var presenter = NativeUi.ByName<ContentPresenter>(button, "NavigationPresenter");
                    var common = VisualStateManager.GetVisualStateGroups(presenter).Single(group => group.Name == "CommonStates");
                    string label = NativeUi.Descendants<TextBlock>(button).Single().Text;
                    string key = "navigation-" + theme.Key + "-" + label;
                    Prompt.Focus(FocusState.Programmatic);
                    MovePointerInside(Element<Grid>("ShellGrid"), 0.75, 0.025);
                    await WaitAsync(() => common.CurrentState?.Name == "Normal" &&
                        presenter.Background is SolidColorBrush idle && idle.Color.A == 0,
                        key + " returns to its actual transparent native normal state");
                    await Task.Delay(120);
                    Check(button.Focus(FocusState.Keyboard) && button.FocusState == FocusState.Keyboard &&
                        ReferenceEquals(FocusManager.GetFocusedElement(_shell.XamlRoot), button) && button.UseSystemFocusVisuals &&
                        ReferenceEquals(button.FocusVisualPrimaryBrush, focus),
                        key + " keeps native keyboard focus and the current palette focus ring");
                    Prompt.Focus(FocusState.Programmatic);
                    var enter = await SampleSidebarNavigationPixelsAsync(button, key, "enter", selection.Color.ToString(),
                        () => MovePointerInside(button), pixelTraces);
                    await WaitAsync(() => common.CurrentState?.Name == "PointerOver" && ReferenceEquals(presenter.Background, selection),
                        key + " receives real mouse hover and paints the current palette soft accent");
                    CheckSidebarNavigationPixelTransition(enter);
                    Check(ReferenceEquals(presenter.Foreground, AppearanceService.GetBrush("KynxaTextBrush")) &&
                        presenter.CornerRadius.Equals(button.CornerRadius),
                        key + " retains themed text and the same rounded surface while hovered");
                    var leave = await SampleSidebarNavigationPixelsAsync(button, key, "leave", AppearanceService.Current.Sidebar,
                        () => MovePointerInside(Element<Grid>("ShellGrid"), 0.75, 0.025), pixelTraces);
                    await WaitAsync(() => common.CurrentState?.Name == "Normal" &&
                        presenter.Background is SolidColorBrush idle && idle.Color.A == 0,
                        key + " returns to normal after the observed native pointer leave");
                    CheckSidebarNavigationPixelTransition(leave);
                    var reenter = await SampleSidebarNavigationPixelsAsync(button, key, "reenter", selection.Color.ToString(),
                        () => MovePointerInside(button), pixelTraces);
                    await WaitAsync(() => common.CurrentState?.Name == "PointerOver" && ReferenceEquals(presenter.Background, selection),
                        key + " restores its actual hover brush after native pointer reentry");
                    CheckSidebarNavigationPixelTransition(reenter);
                    // Inspect the real pressed visual state without invoking a navigation action.
                    // 进入真实按下视觉状态核对画刷，不点击或执行任何导航动作。
                    Check(VisualStateManager.GoToState(button, "Pressed", false), key + " exposes its native pressed visual state");
                    await WaitAsync(() => common.CurrentState?.Name == "Pressed" && ReferenceEquals(presenter.Background, selection),
                        key + " uses the same palette feedback while pressed");
                    observations[key] = new
                    {
                        label, theme.Palette, theme.Mode, theme.Accent,
                        background = selection.Color.ToString(), foreground = ((SolidColorBrush)presenter.Foreground).Color.ToString(),
                        focus = focus.Color.ToString(), presenter.CornerRadius, realMouseHover = true, pressedVisualState = true,
                        renderedEnterLeaveReentry = true
                    };
                    Check(VisualStateManager.GoToState(button, "PointerOver", false), key + " restores hover after its pressed visual check");
                    lastHovered = presenter;
                }
                await CapturePresentedAsync("shell-sidebar-navigation-" + theme.Key + ".png");
            }
        }
        finally
        {
            Prompt.Focus(FocusState.Programmatic);
            AppearanceService.ApplyPreferences(palette, mode, fontSize, accent);
            Window.AppWindow.MoveAndResize(new Windows.Graphics.RectInt32(originalPosition.X, originalPosition.Y,
                originalSize.Width, originalSize.Height));
            if (GetForegroundWindow() == windowHandle) SetCursorPos(originalPointer.X, originalPointer.Y);
            await SettleAsync();
        }
    }

    private sealed record SidebarNavigationPixelSample(double ElapsedMs, double R, double G, double B, double Brightness);
    private sealed record SidebarNavigationPixelTrace(string Key, string Phase, int ScreenX, int ScreenY,
        double RasterizationScale, string ExpectedBrushColor, string? FinalVisualState,
        SidebarNavigationPixelSample Before, List<SidebarNavigationPixelSample> Frames);

    private static double SidebarNavigationPixelDifference(SidebarNavigationPixelSample first, SidebarNavigationPixelSample second) =>
        Math.Max(Math.Abs(first.R - second.R), Math.Max(Math.Abs(first.G - second.G), Math.Abs(first.B - second.B)));

    private async Task<SidebarNavigationPixelTrace> SampleSidebarNavigationPixelsAsync(Button button, string key, string phase,
        string expectedBrushColor, Action movePointer, List<SidebarNavigationPixelTrace> traces)
    {
        RequireFixtureForeground();
        if (button.ActualWidth < 80 || button.ActualHeight < 28)
            throw new InvalidOperationException("The navigation pixel probe has no empty interior patch: " + key);
        // Read one empty interior pixel below the text; repeated GetPixel calls would delay delivery of native hover input.
        // 读取文字下方的单个空白内部像素；反复调用 GetPixel 会延迟原生悬停输入的处理。
        Point point = button.TransformToVisual((FrameworkElement)Window.Content)
            .TransformPoint(new Point(button.ActualWidth - 24, button.ActualHeight - 6));
        double scale = _shell.XamlRoot.RasterizationScale;
        var screen = new NativePoint { X = (int)Math.Round(point.X * scale), Y = (int)Math.Round(point.Y * scale) };
        nint owned = WinRT.Interop.WindowNative.GetWindowHandle(Window);
        if (!ClientToScreen(owned, ref screen) || GetAncestor(WindowFromPoint(screen), 2) != owned)
            throw new InvalidOperationException("The navigation pixel probe is outside the visible owned window: " + key);
        nint screenDc = GetSidebarScreenDc(0);
        if (screenDc == 0) throw new InvalidOperationException("Cannot acquire the composed desktop pixels for " + key);
        var frames = new List<SidebarNavigationPixelSample>();
        try
        {
            SidebarNavigationPixelSample ReadPixel(Stopwatch? observationClock = null)
            {
                RequireFixtureForeground();
                if (GetAncestor(WindowFromPoint(screen), 2) != owned)
                    throw new InvalidOperationException("The owned navigation pixel probe became covered: " + key);
                uint pixel = GetSidebarScreenPixel(screenDc, screen.X, screen.Y);
                if (pixel == uint.MaxValue) throw new InvalidOperationException("Cannot read a composed navigation pixel: " + key);
                double red = pixel & 255, green = (pixel >> 8) & 255, blue = (pixel >> 16) & 255;
                return new(observationClock?.Elapsed.TotalMilliseconds ?? -1, red, green, blue, .2126 * red + .7152 * green + .0722 * blue);
            }
            var before = ReadPixel();
            var elapsed = Stopwatch.StartNew();
            movePointer();
            frames.Add(ReadPixel(elapsed));
            // Yield to XAML between screen reads; no settle delay or composition flush can hide the first transition frames.
            // 屏幕读取之间让出 UI 线程；不先等待稳定或刷新合成，避免跳过最早的过渡帧。
            while (elapsed.Elapsed.TotalMilliseconds < 600 &&
                (elapsed.Elapsed.TotalMilliseconds < 200 || frames.Count < 8 ||
                 !frames.TakeLast(3).All(frame => SidebarNavigationPixelDifference(frame, frames[^1]) <= 3)))
            {
                await Task.Delay(8);
                frames.Add(ReadPixel(elapsed));
            }
            var presenter = NativeUi.ByName<ContentPresenter>(button, "NavigationPresenter");
            var common = VisualStateManager.GetVisualStateGroups(presenter).Single(group => group.Name == "CommonStates");
            var trace = new SidebarNavigationPixelTrace(key, phase, screen.X, screen.Y, scale, expectedBrushColor,
                common.CurrentState?.Name, before, frames);
            traces.Add(trace);
            // Persist every observed frame before checking it so a failing baseline retains the actual transient colors.
            // 断言前保存每个已观察帧，让失败的基线也保留真实瞬态颜色。
            File.WriteAllText(Path.Combine(_directory, "shell-sidebar-navigation-pixels.json"), JsonSerializer.Serialize(new
            {
                targetIntervalMs = 8, minimumObservationMs = 200, maximumObservationMs = 600,
                patch = "one composed screen pixel", endpointTolerance = 3, traces
            }, new JsonSerializerOptions { WriteIndented = true }));
            return trace;
        }
        finally { ReleaseSidebarScreenDc(0, screenDc); }
    }

    private void CheckSidebarNavigationPixelTransition(SidebarNavigationPixelTrace trace)
    {
        const double tolerance = 3;
        var final = trace.Frames[^1];
        Check(trace.Frames.Count >= 8 && final.ElapsedMs >= 200 &&
            trace.Frames.TakeLast(3).All(frame => SidebarNavigationPixelDifference(frame, final) <= tolerance),
            trace.Key + " " + trace.Phase + " reaches a stable actually rendered endpoint after immediate native pointer input");
        // Bounds come from displayed idle/hover endpoints, so screen color conversion cannot create a false flash result.
        // 边界取自实际显示的空闲／悬停端点，避免屏幕颜色转换被误判为闪白。
        bool Within(double value, double first, double last) => value >= Math.Min(first, last) - tolerance &&
            value <= Math.Max(first, last) + tolerance;
        Check(trace.Frames.All(frame => Within(frame.R, trace.Before.R, final.R) &&
            Within(frame.G, trace.Before.G, final.G) && Within(frame.B, trace.Before.B, final.B) &&
            frame.Brightness <= Math.Max(trace.Before.Brightness, final.Brightness) + tolerance),
            trace.Key + " " + trace.Phase + " has no transient white or brightness flash outside actual idle/hover endpoints; " +
            JsonSerializer.Serialize(new { trace.Before, final, peakBrightness = trace.Frames.Max(frame => frame.Brightness),
                evidence = "shell-sidebar-navigation-pixels.json" }));
    }

    [DllImport("user32.dll", EntryPoint = "GetDC")] private static extern nint GetSidebarScreenDc(nint windowHandle);
    [DllImport("user32.dll", EntryPoint = "ReleaseDC")] private static extern int ReleaseSidebarScreenDc(nint windowHandle, nint deviceContext);
    [DllImport("gdi32.dll", EntryPoint = "GetPixel")] private static extern uint GetSidebarScreenPixel(nint deviceContext, int x, int y);

    private void CheckSidebarHeaderGeometry(string key, double expectedWidth, Dictionary<string, object> observations)
    {
        var shell = Element<Grid>("ShellGrid");
        var pane = Element<Grid>("SidebarPane");
        var header = Element<Grid>("GlobalSidebarHeader");
        var switcher = Element<Border>("ChatWorkSwitcher");
        var collapse = Element<Button>("CollapseSidebarButton");
        var expand = Element<Button>("CompactSidebarButton");
        var shellBrush = Element<Grid>("ShellGrid").Background as SolidColorBrush;
        var sidebarBrush = pane.Background as SolidColorBrush;
        var mainBrush = Element<Grid>("MainRegion").Background as SolidColorBrush;
        var grip = Element<ResizeGrip>("SidebarGrip");
        var footer = Element<Grid>("SidebarFooter");
        var settings = NativeUi.Descendants<Button>(footer).Single(button =>
            AutomationProperties.GetName(button) == UiText.Get("设置"));
        var settingsIcon = (ThemeIcon)settings.Content;
        var monitorIcon = NativeUi.Descendants<ThemeIcon>(Element<Button>("ModelStatusButton"))
            .Single(icon => icon.IconKey == "IconDeviceStatus");
        Rect Bounds(FrameworkElement element, FrameworkElement relativeTo) => element.TransformToVisual(relativeTo)
            .TransformBounds(new Rect(0, 0, element.ActualWidth, element.ActualHeight));
        bool Inside(FrameworkElement element, FrameworkElement relativeTo)
        {
            Rect box = Bounds(element, relativeTo);
            return box.Width > 0 && box.Height > 0 && box.Left >= -0.5 && box.Top >= -0.5 &&
                box.Right <= relativeTo.ActualWidth + 0.5 && box.Bottom <= relativeTo.ActualHeight + 0.5;
        }
        Check(Math.Abs(pane.ActualWidth - expectedWidth) < 1 && NativeUi.IsVisible(collapse) && NativeUi.IsVisible(switcher) &&
            !NativeUi.IsVisible(expand), key + " has the requested actual pane width and mutually exclusive visible controls");
        Rect paneBounds = Bounds(pane, shell), mainBounds = Bounds(Element<Grid>("MainRegion"), shell);
        Rect gripBounds = Bounds(grip, shell), settingsBounds = Bounds(settingsIcon, shell), monitorBounds = Bounds(monitorIcon, shell);
        double leftFooterGap = settingsBounds.Left - paneBounds.Left;
        double rightFooterGap = mainBounds.Left - monitorBounds.Right;
        // Wide mode reserves a grip gutter; the compact drawer overlays the unchanged collapsed main-content origin.
        // 宽布局预留拖动留白；紧凑抽屉覆盖正文，不改变其折叠模式起点。
        if (!ShellField<bool>("_isCompactLayout"))
        {
            Check(Math.Abs(paneBounds.Left) < .5 && Math.Abs(mainBounds.Left - expectedWidth - 4) < .5 &&
                Math.Abs(gripBounds.Width - 8) < .5 && Math.Abs(gripBounds.Left - expectedWidth + 2) < .5 &&
                Math.Abs(gripBounds.Right - expectedWidth - 6) < .5 &&
                Math.Abs(paneBounds.Right - gripBounds.Left - 2) < .5 && Math.Abs(gripBounds.Right - mainBounds.Left - 2) < .5 &&
                Canvas.GetZIndex(grip) > Canvas.GetZIndex(pane),
                key + " reserves only four DIPs before main content while the eight-DIP grip overlaps both adjacent regions by two DIPs");
            Check(Math.Abs(settingsBounds.Left - 20) < .5 && Math.Abs(settingsBounds.Right - 40) < .5 &&
                Math.Abs(monitorBounds.Left - expectedWidth + 36) < .5 && Math.Abs(monitorBounds.Right - expectedWidth + 16) < .5 &&
                Math.Abs(leftFooterGap - 20) < .5 && Math.Abs(rightFooterGap - 20) < .5 && Math.Abs(leftFooterGap - rightFooterGap) < .5,
                key + " preserves both original footer icon anchors and equal twenty-DIP actual outer margins");
        }
        Check(shellBrush is not null && sidebarBrush is not null && mainBrush is not null &&
            shellBrush.Color.A == 255 && shellBrush.Color == sidebarBrush.Color && mainBrush.Color.A == 255,
            key + " fills the transparent grip column with the sidebar color while the main region retains its own opaque background");
        Check(Inside(header, pane) && Inside(switcher, header) && Inside(collapse, header) &&
            Bounds(switcher, header).Right <= Bounds(collapse, header).Left - 3 && collapse.ActualWidth >= 32,
            key + " keeps the mode switcher and complete close target inside the header without clipping or overlap");
        foreach (string name in new[] { "WorkModeButton", "ChatModeButton" })
        {
            var mode = Element<Button>(name);
            var content = (StackPanel)mode.Content;
            var label = content.Children.OfType<TextBlock>().Single();
            var icon = content.Children.OfType<FontIcon>().Single();
            // The horizontal content stack measures natural text width in the real XAML tree.
            // 横向内容容器在真实 XAML 树中测量文字自然宽度，避免脱离视图的语言和像素取整差异。
            bool fits = Inside(mode, switcher) && Inside(label, mode) && Inside(icon, mode) &&
                !label.IsTextTrimmed && !string.IsNullOrWhiteSpace(label.Text) &&
                Bounds(icon, mode).Right <= Bounds(label, mode).Left + 0.5;
            string diagnostic = fits ? "" : JsonSerializer.Serialize(new
            {
                label.Text, label.ActualWidth, label.IsTextTrimmed, modeBounds = Bounds(mode, switcher),
                labelBounds = Bounds(label, mode), iconBounds = Bounds(icon, mode),
                label.FontSize, label.Language
            });
            Check(fits, key + " fits the complete localized " + name + " label and icon within its own button " + diagnostic);
        }
        Check(AutomationProperties.GetName(collapse) == UiText.Get("收起侧栏") &&
            ToolTipService.GetToolTip(collapse)?.ToString() == UiText.Get("收起侧栏") && collapse.IsTabStop,
            key + " retains the localized accessible name, explanatory tooltip and keyboard focus target");
        Check(collapse.Content is ThemeIcon { IconKey: "IconSidebarToggle", IsHitTestVisible: false, IsTabStop: false } collapseIcon &&
            Math.Abs(collapseIcon.ActualWidth - 20) < 0.5 && Math.Abs(collapseIcon.ActualHeight - 20) < 0.5 &&
            ReferenceEquals(collapseIcon.Foreground, AppearanceService.GetBrush("KynxaIconBrush")),
            key + " renders the shared themed panel icon inside the existing native button without an extra hit or focus target");
        observations[key] = new
        {
            pane = new { pane.ActualWidth, pane.ActualHeight }, header = Bounds(header, pane),
            main = mainBounds, grip = gripBounds, footer = new { settings = settingsBounds, monitor = monitorBounds,
                leftOuterGap = leftFooterGap, rightOuterGap = rightFooterGap },
            backgrounds = new { shell = shellBrush?.Color.ToString(), sidebar = sidebarBrush?.Color.ToString(), main = mainBrush?.Color.ToString() },
            switcher = Bounds(switcher, header), collapse = Bounds(collapse, header),
            work = Bounds(Element<Button>("WorkModeButton"), header), chat = Bounds(Element<Button>("ChatModeButton"), header),
            labels = new[] { "WorkModeButton", "ChatModeButton" }.Select(name => new
            {
                name, texts = ((StackPanel)Element<Button>(name).Content).Children.OfType<TextBlock>()
                    .Select(text => new { text.Text, bounds = Bounds(text, header) }).ToArray()
            }).ToArray()
        };
    }

    private async Task CheckSidebarGripOverlapHitTestsAsync(string key, Dictionary<string, object> observations)
    {
        // Layout and window resizing do not guarantee foreground ownership; activate immediately before native input.
        // 布局和窗口尺寸更新不保证前台归属；原生输入前重新激活已核验的自有窗口。
        try
        {
            await ActivateOwnedShortcutWindowAsync();
            RequireFixtureForeground();
        }
        catch
        {
            nint foreground = GetForegroundWindow();
            GetSettingsWindowProcessId(foreground, out uint foregroundProcessId);
            File.WriteAllText(Path.Combine(_directory, "shell-sidebar-grip-foreground.json"), JsonSerializer.Serialize(new
            {
                key, ownedWindow = WinRT.Interop.WindowNative.GetWindowHandle(Window).ToInt64(),
                ownedProcessId = Environment.ProcessId, foregroundWindow = foreground.ToInt64(), foregroundProcessId
            }, new JsonSerializerOptions { WriteIndented = true }));
            throw;
        }
        var grip = Element<ResizeGrip>("SidebarGrip");
        var shell = Element<Grid>("ShellGrid");
        var windowRoot = (FrameworkElement)Window.Content;
        var pointerOver = typeof(ResizeGrip).GetField("_pointerOver", PrivateInstance)!;
        var dragging = typeof(ResizeGrip).GetField("_dragging", PrivateInstance)!;
        bool IsOver() => (bool)pointerOver.GetValue(grip)!;
        Rect pane = Element<Grid>("SidebarPane").TransformToVisual(shell)
            .TransformBounds(new Rect(0, 0, Element<Grid>("SidebarPane").ActualWidth, Element<Grid>("SidebarPane").ActualHeight));
        Rect main = Element<Grid>("MainRegion").TransformToVisual(shell)
            .TransformBounds(new Rect(0, 0, Element<Grid>("MainRegion").ActualWidth, Element<Grid>("MainRegion").ActualHeight));
        var hits = new List<object>();
        // Move into each two-DIP overlap from outside the grip; real PointerEntered must reach the grip above both neighbors.
        // 从拖动区外侧分别移入两个 2 DIP 重叠区；真实 PointerEntered 必须穿过相邻区域并到达上层拖动控件。
        foreach (var side in new[] { (Name: "sidebar", Fraction: .125), (Name: "main", Fraction: .875) })
        {
            MovePointerInside(shell, .75, .025);
            await WaitAsync(() => !IsOver(), key + " leaves the real sidebar grip before testing its " + side.Name + " overlap");
            MovePointerInside(grip, side.Fraction, .5);
            await WaitAsync(IsOver, key + " receives a real pointer enter inside its two-DIP " + side.Name + " overlap");
            if (!GetCursorPos(out NativePoint actual)) throw new InvalidOperationException("Cannot read the actual grip overlap pointer.");
            var origin = new NativePoint();
            nint owned = WinRT.Interop.WindowNative.GetWindowHandle(Window);
            if (!ClientToScreen(owned, ref origin)) throw new InvalidOperationException("Cannot locate the sidebar overlap client origin.");
            double scale = _shell.XamlRoot.RasterizationScale;
            Point clientPoint = new((actual.X - origin.X) / scale, (actual.Y - origin.Y) / scale);
            Point shellPoint = windowRoot.TransformToVisual(shell).TransformPoint(clientPoint);
            bool inOverlap = side.Name == "sidebar"
                ? shellPoint.X > pane.Right - 2 && shellPoint.X < pane.Right
                : shellPoint.X > main.Left && shellPoint.X < main.Left + 2;
            Check(inOverlap && !(bool)dragging.GetValue(grip)! && GetAncestor(WindowFromPoint(actual), 2) == owned,
                key + " hits the actual " + side.Name + " overlap screen point without pressing or dragging");
            hits.Add(new { side.Name, side.Fraction, screen = new { actual.X, actual.Y }, clientPoint, shellPoint,
                scale, realPointerEntered = IsOver(), dragging = false });
        }
        MovePointerInside(shell, .75, .025);
        await WaitAsync(() => !IsOver(), key + " clears native grip hover after both overlap probes");
        observations[key + "-grip-overlap-hits"] = hits;
    }

    private void CheckCollapsedSidebarGeometry(string key, Dictionary<string, object> observations)
    {
        var shell = Element<Grid>("ShellGrid");
        var pane = Element<Grid>("SidebarPane");
        var expand = Element<Button>("CompactSidebarButton");
        var footer = Element<Grid>("SidebarFooter");
        var settings = NativeUi.Descendants<Button>(footer).Single(button =>
            AutomationProperties.GetName(button) == UiText.Get("设置"));
        var settingsIcon = (ThemeIcon)settings.Content;
        Rect Bounds(FrameworkElement element) => element.TransformToVisual(shell)
            .TransformBounds(new Rect(0, 0, element.ActualWidth, element.ActualHeight));
        Rect expandBounds = Bounds(expand);
        Rect settingsBounds = Bounds(settingsIcon);
        Rect mainBounds = Bounds(Element<Grid>("MainRegion"));
        double expandCenter = expandBounds.Left + expandBounds.Width / 2;
        double settingsCenter = settingsBounds.Left + settingsBounds.Width / 2;
        // Measure the complete rail through the content edge, including any dormant resize column.
        // 测量到正文左缘的整条窄侧栏，包含可能残留的不可见拖动列。
        Check(Math.Abs(expandCenter - 24) < 0.5 && Math.Abs(settingsCenter - 24) < 0.5,
            key + " preserves the original 24-DIP horizontal centers of the hamburger and settings icon");
        Check(Math.Abs(mainBounds.Left - 2 * expandCenter) < 0.5 &&
            Math.Abs(settingsBounds.Left - (mainBounds.Left - settingsBounds.Right)) < 0.5 &&
            Math.Abs(pane.ActualWidth - mainBounds.Left) < 0.5 && !NativeUi.IsVisible(Element<ResizeGrip>("SidebarGrip")),
            key + " has equal left/right rail margins and no unused resize gutter before the main content");
        Check(expandBounds.Left >= 0 && expandBounds.Right <= pane.ActualWidth + 0.5 &&
            settingsBounds.Left >= 0 && settingsBounds.Right <= pane.ActualWidth + 0.5,
            key + " keeps both original controls fully inside the narrower rail");
        observations[key] = new { pane.ActualWidth, expand = expandBounds, settings = settingsBounds,
            main = mainBounds, expandCenter, settingsCenter };
    }

    private static void RaiseSidebarGripEvent(ResizeGrip grip, string eventName, EventArgs args)
    {
        var subscribed = typeof(ResizeGrip).GetField(eventName, BindingFlags.Instance | BindingFlags.NonPublic)?.GetValue(grip) as Delegate
            ?? throw new InvalidOperationException("The real sidebar grip has no subscribed production handler for " + eventName);
        subscribed.DynamicInvoke(grip, args);
    }
}

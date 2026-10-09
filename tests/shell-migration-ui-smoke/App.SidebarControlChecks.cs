using System.Reflection;
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
        var layout = ShellField<LayoutState>("_layout");
        double originalWidth = layout.SidebarWidth;
        bool originallyCollapsed = layout.SidebarCollapsed;
        string originalLanguage = UiText.Language;
        string originalDraft = Prompt.Text;
        Guid? originalChat = ActiveId;
        var originalRows = _shell.ActiveMessages.ToArray();
        string originalMessages = JsonSerializer.Serialize(originalRows.Select(row => row.Message).ToArray());
        var originalSize = Window.AppWindow.Size;
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
                    if (language == "zh-CN" && width == ShellLayoutMetrics.SidebarDefault)
                        await CapturePresentedAsync("shell-sidebar-controls-wide.png");
                    if (language == "en" && width == ShellLayoutMetrics.SidebarMin)
                        await CapturePresentedAsync("shell-sidebar-controls-minimum-200-en.png");
                }
            }
            CheckContext("localized default, minimum and maximum header layouts preserve draft, active messages and the formal catalog");

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
            await CapturePresentedAsync("shell-sidebar-controls-closed.png");
            expand.Focus(FocusState.Keyboard);
            NativeUi.Invoke(expand);
            await WaitAsync(() => NativeUi.IsVisible(collapse) && !NativeUi.IsVisible(expand) && Math.Abs(pane.ActualWidth - 318) < 1,
                "the real expand button restores the new collapse counterpart after native layout");
            Check(ReferenceEquals(FocusManager.GetFocusedElement(_shell.XamlRoot), collapse) && collapse.FocusState == FocusState.Keyboard,
                "wide native expansion preserves keyboard focus and its focus ring on the visible collapse button");
            Check(SameSavedSidebar(318, false) && Math.Abs(pane.ActualWidth - 318) < 1,
                "wide expansion saves the open preference and restores the previous user width");

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
            CheckContext("sidebar buttons, grip handlers, localization and compact dismissals preserve the exact draft, messages and zero-write catalog");
            File.WriteAllText(Path.Combine(_directory, "shell-sidebar-control-geometry.json"),
                JsonSerializer.Serialize(observations, new JsonSerializerOptions { WriteIndented = true }));
        }
        finally
        {
            // Restore only fixture-owned UI preferences and its original draft; no formal messages are edited.
            // 仅恢复夹具自己的 UI 偏好和原草稿，不编辑正式消息。
            UiText.Initialize(originalLanguage);
            layout.SidebarWidth = originalWidth;
            layout.SidebarCollapsed = originallyCollapsed;
            SetShellField("_temporarySidebarOpen", false);
            Window.AppWindow.Resize(originalSize);
            Call("ApplyLayout");
            Call("SaveLayout");
            NativeUi.SetText(Prompt, originalDraft);
            await SettleAsync();
        }
    }

    private void CheckSidebarHeaderGeometry(string key, double expectedWidth, Dictionary<string, object> observations)
    {
        var pane = Element<Grid>("SidebarPane");
        var header = Element<Grid>("GlobalSidebarHeader");
        var switcher = Element<Border>("ChatWorkSwitcher");
        var collapse = Element<Button>("CollapseSidebarButton");
        var expand = Element<Button>("CompactSidebarButton");
        var shellBrush = Element<Grid>("ShellGrid").Background as SolidColorBrush;
        var sidebarBrush = pane.Background as SolidColorBrush;
        var mainBrush = Element<Grid>("MainRegion").Background as SolidColorBrush;
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
        observations[key] = new
        {
            pane = new { pane.ActualWidth, pane.ActualHeight }, header = Bounds(header, pane),
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

    private static void RaiseSidebarGripEvent(ResizeGrip grip, string eventName, EventArgs args)
    {
        var subscribed = typeof(ResizeGrip).GetField(eventName, BindingFlags.Instance | BindingFlags.NonPublic)?.GetValue(grip) as Delegate
            ?? throw new InvalidOperationException("The real sidebar grip has no subscribed production handler for " + eventName);
        subscribed.DynamicInvoke(grip, args);
    }
}

using System.Runtime.InteropServices;
using System.Text.Json;
using KYNXA.Contracts;
using KYNXA_Desktop.Controls;
using KYNXA_Desktop.Models.UI;
using KYNXA_Desktop.Services;
using KYNXA_Desktop.ViewModels;
using MemoryUiSmoke;
using Microsoft.UI.Windowing;
using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Controls;
using Microsoft.UI.Xaml.Controls.Primitives;
using Microsoft.UI.Xaml.Input;
using Microsoft.UI.Xaml.Media;
using Windows.Foundation;

namespace KYNXA_Desktop;

public partial class App
{
    private readonly List<object> _shortcutPointerObservations = [];
    private readonly List<object> _shortcutPopupObservations = [];
    private static string TooltipText(object? content) => content is TextBlock text ? text.Text : content?.ToString() ?? "";

    private Popup[] OpenNativePopups() => VisualTreeHelper.GetOpenPopupsForXamlRoot(_shell.XamlRoot)
        .Where(popup => popup.IsOpen && popup.Child is not null).ToArray();

    private ToolTip[] OpenNativeTooltips() => OpenNativePopups()
        .SelectMany(popup => NativeUi.Descendants<ToolTip>(popup.Child!)).Where(tooltip => tooltip.IsOpen).ToArray();

    private string[] VisibleNativePopupText() => OpenNativePopups().Select(popup =>
        string.Join(" ", NativeUi.Descendants<TextBlock>(popup.Child!).Where(NativeUi.IsVisible)
            .Select(text => text.Text).Where(text => !string.IsNullOrWhiteSpace(text)))).ToArray();

    private static string WithoutTooltipWhitespace(string text) => new(text.Where(character => !char.IsWhiteSpace(character)).ToArray());

    private bool HasNativePopupText(string expected) => VisibleNativePopupText().Any(text =>
        WithoutTooltipWhitespace(text).Equals(WithoutTooltipWhitespace(expected), StringComparison.Ordinal));

    private bool HasBareShortcutTooltip() => VisibleNativePopupText().Any(text =>
        WithoutTooltipWhitespace(text).Equals("Ctrl+F", StringComparison.OrdinalIgnoreCase));

    private void RecordShortcutPopupObservation(string stage, Button search)
    {
        // ToolTip may own an internal Presenter as its Popup child; observe actual visible text without assuming its type.
        // ToolTip 的 Popup 子节点可能是内部 Presenter；观察真实可见文本，不假设子节点必须是 ToolTip 类型。
        _shortcutPopupObservations.Add(new
        {
            Stage = stage,
            Time = DateTimeOffset.UtcNow,
            SearchIsPointerOver = search.IsPointerOver,
            SearchFocusState = search.FocusState.ToString(),
            PopupChildren = OpenNativePopups().Select(popup => new
            {
                ChildType = popup.Child!.GetType().FullName,
                PopupIsOpen = popup.IsOpen,
                VisibleTexts = NativeUi.Descendants<TextBlock>(popup.Child!).Where(NativeUi.IsVisible)
                    .Select(text => text.Text).ToArray(),
                AllTextBlocks = NativeUi.Descendants<TextBlock>(popup.Child!).Select(text => new
                {
                    text.Text, IsVisible = NativeUi.IsVisible(text), text.ActualWidth, text.ActualHeight,
                    Visibility = text.Visibility.ToString()
                }).ToArray(),
                Tooltips = NativeUi.Descendants<ToolTip>(popup.Child!).Select(tooltip => new
                    { Text = TooltipText(tooltip.Content), tooltip.IsOpen, tooltip.IsEnabled }).ToArray()
            }).ToArray()
        });
        File.WriteAllText(Path.Combine(_directory, "shell-shortcut-tooltip-popups.json"),
            JsonSerializer.Serialize(_shortcutPopupObservations, new JsonSerializerOptions { WriteIndented = true }));
    }

    private async Task CheckShortcutTooltipsAsync()
    {
        if (ActiveId is null) await SearchAndSelectAsync(_gateway.WorkChatId, "Work fixture");
        var search = Element<Button>("HistorySearchButton");
        var transcript = Element<ConversationTranscript>("ConversationMessages");
        string language = UiText.Language;
        Guid? chatId = ActiveId;
        string draft = Prompt.Text;
        var messages = _shell.ActiveMessages.Select(row => row.Message).ToArray();
        string catalog = JsonSerializer.Serialize(_gateway.Catalog);
        nint windowHandle = WinRT.Interop.WindowNative.GetWindowHandle(Window);
        var originalPosition = Window.AppWindow.Position;
        var originalSize = Window.AppWindow.Size;
        if (!GetCursorPos(out NativePoint originalPointer)) throw new InvalidOperationException("Cannot read the original pointer position.");
        try
        {
            Window.AppWindow.Show(activateWindow: true);
            Window.Activate();
            SetForegroundWindow(windowHandle);
            await WaitAsync(() => GetForegroundWindow() == windowHandle,
                "only the fixture's owned native window is foreground before sending shortcut input");

            // Keep every native target physically on screen; XAML visibility alone cannot prove pointer reachability.
            // 确保原生目标真实位于屏幕内；仅有 XAML 可见性不足以证明指针能到达。
            var workArea = DisplayArea.GetFromWindowId(Window.AppWindow.Id, DisplayAreaFallback.Primary).WorkArea;
            double scale = _shell.XamlRoot.RasterizationScale;
            int width = Math.Min((int)Math.Ceiling(1280 * scale), workArea.Width - 32);
            int height = Math.Min((int)Math.Ceiling(740 * scale), workArea.Height - 32);
            Window.AppWindow.MoveAndResize(new Windows.Graphics.RectInt32(workArea.X + 16, workArea.Y + 16, width, height));
            await WaitAsync(() => Element<Grid>("ShellGrid").ActualWidth >= 980 &&
                _shell.XamlRoot.Size.Width <= width / scale + 1 && _shell.XamlRoot.Size.Height <= height / scale + 1,
                "the fixture reaches a wide layout that physically fits the monitor work area");
            await SettleAsync();
            File.WriteAllText(Path.Combine(_directory, "shell-shortcut-tooltip-window.json"), JsonSerializer.Serialize(new
            {
                OriginalPosition = new { originalPosition.X, originalPosition.Y },
                OriginalSize = new { originalSize.Width, originalSize.Height },
                WorkArea = new { workArea.X, workArea.Y, workArea.Width, workArea.Height },
                Position = new { Window.AppWindow.Position.X, Window.AppWindow.Position.Y },
                Size = new { Window.AppWindow.Size.Width, Window.AppWindow.Size.Height },
                RasterizationScale = scale
            }, new JsonSerializerOptions { WriteIndented = true }));

            // Observe WinUI's real attached tooltip and popup before asserting absence, including the failing version.
            // 在断言前记录 WinUI 真实附加提示和弹出层，未修复版本也保留现场证据。
            var pageTooltip = ToolTipService.GetToolTip(_shell);
            // Enter native XAML chrome first; a pointer confined to WebView can bypass the Page's hover event.
            // 先进入原生 XAML 顶部区域；仅在 WebView 内移动可能绕过 Page 悬停事件。
            MovePointerInside(Element<Grid>("ShellGrid"), 0.75, 0.025);
            RecordShortcutPopupObservation("native-header-enter", search);
            bool sawBareTooltip = false;
            DateTime hoverDeadline = DateTime.UtcNow.AddMilliseconds(3000);
            while (DateTime.UtcNow < hoverDeadline)
            {
                RequireFixtureForeground();
                sawBareTooltip |= HasBareShortcutTooltip();
                await Task.Delay(30);
            }
            RecordShortcutPopupObservation("native-header-after-hover", search);
            File.WriteAllText(Path.Combine(_directory, "shell-shortcut-tooltip-observation.json"), JsonSerializer.Serialize(new
            {
                PageTooltipType = pageTooltip?.GetType().FullName,
                PageTooltipContent = pageTooltip is ToolTip actual ? TooltipText(actual.Content) : TooltipText(pageTooltip),
                PageTooltipEnabled = pageTooltip is ToolTip actualTooltip ? actualTooltip.IsEnabled : (bool?)null,
                PagePlacement = _shell.KeyboardAcceleratorPlacementMode.ToString(),
                SawBareCtrlFPopup = sawBareTooltip,
                OpenTooltipContents = OpenNativeTooltips().Select(tooltip => TooltipText(tooltip.Content)).ToArray(),
                OpenPopupVisibleTexts = VisibleNativePopupText()
            }, new JsonSerializerOptions { WriteIndented = true }));
            await NativeWindowCapture.CaptureAsync(Window, Path.Combine(_directory, "shell-shortcut-tooltip-hover.png"));
            bool sawBareDuringScroll = await CheckScrolledTranscriptTooltipAsync(transcript, chatId!.Value);
            Check(!sawBareTooltip && !sawBareDuringScroll && !HasBareShortcutTooltip(),
                "hovering the real native Shell chrome then scrolling its long transcript does not display a page-wide Ctrl+F popup");

            foreach (string currentLanguage in new[] { "zh-CN", "en" })
            {
                UiText.Initialize(currentLanguage);
                await SettleAsync();
                string expected = UiText.Get("搜索会话 Ctrl+Shift+K");
                Check(TooltipText(ToolTipService.GetToolTip(search)) == expected,
                    $"the explicit search button tooltip retains its {currentLanguage} label and shortcut");
                MovePointerInside(search);
                int observationCount = 0;
                await WaitAsync(() =>
                {
                    if (observationCount++ % 30 == 0) RecordShortcutPopupObservation("search-hover-" + currentLanguage, search);
                    return HasNativePopupText(expected);
                },
                    "hovering the real search button opens its explicit localized native tooltip");
                RecordShortcutPopupObservation("search-tooltip-open-" + currentLanguage, search);
                Check(!HasBareShortcutTooltip(),
                    $"the {currentLanguage} search button shows its useful label without the bare page shortcut popup");
                MovePointerInside(Element<Grid>("ShellGrid"), 0.75, 0.025);
                await WaitAsync(() => !HasNativePopupText(expected),
                    "leaving the search button closes its explicit tooltip");
            }
            UiText.Initialize(language);
            await SettleAsync();

            // Dispatch native keys rather than directly invoking production accelerator handlers.
            // 发送原生按键，而非直接调用生产快捷键处理函数。
            search.Focus(FocusState.Programmatic);
            DispatchShortcut(0x46);
            await WaitForTranscriptFindAsync(transcript, open: true, focused: true);
            Check(!HasPopup<TextBox>("HistorySearchBox") && Prompt.Text == draft && ActiveId == chatId,
                "native Ctrl+F opens the production body search without altering the draft or conversation");
            DispatchShortcut(0x46);
            await WaitForTranscriptFindAsync(transcript, open: true, focused: true);
            Check(!HasPopup<TextBox>("HistorySearchBox"),
                "Ctrl+F from the focused WebView retains the same body search and suppresses browser find");
            DispatchShortcut(0x1B, control: false);
            await WaitForTranscriptFindAsync(transcript, open: false);
            await WaitAsync(() => Prompt.FocusState != FocusState.Unfocused,
                "explicit Escape in body search returns focus to the native composer");
            await CheckBodySearchAfterNativeInputAsync(transcript);
            search.Focus(FocusState.Programmatic);
            DispatchShortcut(0x4B, shift: true);
            await WaitAsync(() => HasPopup<TextBox>("HistorySearchBox"), "actual Ctrl+Shift+K opens the production history search");
            Check(Prompt.Text == draft && ActiveId == chatId,
                "native Ctrl+Shift+K opens title/project search without altering the draft or conversation");
            DispatchShortcut(0x1B, control: false);
            await WaitAsync(() => !HasPopup<TextBox>("HistorySearchBox"), "actual Escape closes the history search popup");
            transcript.Browser.Focus(FocusState.Programmatic);
            DispatchShortcut(0x46);
            await WaitForTranscriptFindAsync(transcript, open: true, focused: true);
            int searchRequests = 0;
            int nativeSearchRequests = 0;
            var conversationAccelerator = _shell.KeyboardAccelerators.Single(accelerator => accelerator.Key == Windows.System.VirtualKey.K);
            void NativeSearchRequested(KeyboardAccelerator _, KeyboardAcceleratorInvokedEventArgs __) => nativeSearchRequests++;
            void SearchRequested(object? _, EventArgs __) => searchRequests++;
            transcript.ConversationSearchRequested += SearchRequested;
            conversationAccelerator.Invoked += NativeSearchRequested;
            try
            {
                DispatchShortcut(0x4B, shift: true);
                await WaitAsync(() => HasPopup<TextBox>("HistorySearchBox"),
                    "Ctrl+Shift+K from the focused WebView opens native conversation search");
                await SettleAsync();
                Check(searchRequests + nativeSearchRequests == 1,
                    $"the focused WebView dispatches exactly one conversation search action per Ctrl+Shift+K (browser={searchRequests}, native={nativeSearchRequests})");
                DispatchShortcut(0x1B, control: false);
                await WaitAsync(() => !HasPopup<TextBox>("HistorySearchBox"), "Escape closes WebView-requested history search");
            }
            finally
            {
                transcript.ConversationSearchRequested -= SearchRequested;
                conversationAccelerator.Invoked -= NativeSearchRequested;
            }
            transcript.CloseFindText();
            await WaitForTranscriptFindAsync(transcript, open: false);
            search.Focus(FocusState.Programmatic);
            DispatchShortcut(0x4C);
            await WaitAsync(() => Prompt.FocusState != FocusState.Unfocused, "actual Ctrl+L focuses the production composer");
            Check(Prompt.Text == draft && ActiveId == chatId && !HasBareShortcutTooltip(),
                "native Ctrl+L preserves the draft and does not recreate a page tooltip");

            var tool = new ToolActivity("fixture-shortcut-approval", "workspace.read_file", null,
                "approval-required", "Read the synthetic mounted file", ApprovalId: Guid.NewGuid(),
                WorkspaceRoot: Path.Combine(_directory, "Workspace"));
            var dialog = ToolApprovalDialog.Create(_shell.XamlRoot, tool);
            var showing = dialog.ShowAsync();
            try
            {
                await WaitAsync(() => NativeUi.OpenDialog(_shell) is not null, "the real approval dialog opens before native shortcut guards");
                await WaitAsync(() => Prompt.FocusState == FocusState.Unfocused && FocusManager.GetFocusedElement(_shell.XamlRoot) is not null,
                    "the real approval dialog takes focus before native shortcut guards");
                object? dialogFocus = FocusManager.GetFocusedElement(_shell.XamlRoot);
                DispatchShortcut(0x46);
                await SettleAsync();
                DispatchShortcut(0x4B, shift: true);
                await SettleAsync();
                DispatchShortcut(0x4C);
                await SettleAsync();
                await WaitForTranscriptFindAsync(transcript, open: false);
                Check(!HasPopup<TextBox>("HistorySearchBox") && Prompt.FocusState == FocusState.Unfocused &&
                    ReferenceEquals(FocusManager.GetFocusedElement(_shell.XamlRoot), dialogFocus) &&
                    Prompt.Text == draft && ActiveId == chatId,
                    "actual Ctrl+F, Ctrl+Shift+K and Ctrl+L respect the approval modal and preserve its focus and the original draft");
            }
            finally
            {
                NativeUi.InvokeDialogButton(dialog, primary: false);
                await showing;
            }
            Check(_shell.ActiveMessages.Select(row => row.Message).SequenceEqual(messages) &&
                JsonSerializer.Serialize(_gateway.Catalog) == catalog && _gateway.Writes == 0,
                "native tooltip and shortcut checks preserve message objects and formal catalog without gateway writes");
        }
        finally
        {
            transcript.CloseFindText();
            UiText.Initialize(language);
            (typeof(KYNXA_Desktop.Views.ShellPage).GetField("_historySearchFlyout", PrivateInstance)!.GetValue(_shell) as Flyout)?.Hide();
            Window.AppWindow.MoveAndResize(new Windows.Graphics.RectInt32(originalPosition.X, originalPosition.Y,
                originalSize.Width, originalSize.Height));
            if (GetForegroundWindow() == windowHandle) SetCursorPos(originalPointer.X, originalPointer.Y);
        }
    }

    private async Task CheckEmptyTranscriptFindAsync()
    {
        Check(_shell.ActiveMessages.Count == 0, "the native empty-conversation search fixture starts without message content");
        var transcript = Element<ConversationTranscript>("ConversationMessages");
        string draft = Prompt.Text;
        Guid? chatId = ActiveId;
        double sidebarWidth = Element<Grid>("SidebarPane").ActualWidth;
        // The runner starts hidden; explicitly show only this owned fixture before any native input.
        // 入口以隐藏方式启动；发送原生输入前，仅显式显示本夹具拥有的窗口。
        Window.AppWindow.Show(activateWindow: true);
        Window.Activate();
        SetForegroundWindow(WinRT.Interop.WindowNative.GetWindowHandle(Window));
        await ActivateOwnedShortcutWindowAsync();
        await WaitAsync(() => GetForegroundWindow() == WinRT.Interop.WindowNative.GetWindowHandle(Window),
            "the fixture owns foreground before empty-conversation shortcut input");
        Prompt.Focus(FocusState.Programmatic);
        DispatchShortcut(0x46);
        await WaitForTranscriptFindAsync(transcript, open: true, focused: true);
        await WaitAsync(() => transcript.IsFindTextOpen && NativeUi.IsVisible(transcript),
            "Ctrl+F makes an empty native transcript search surface visible");
        using var count = JsonDocument.Parse(await transcript.Browser.ExecuteScriptAsync("window.kynxaTranscriptSearch.state().count"));
        Check(count.RootElement.GetInt32() == 0 && Prompt.Text == draft && ActiveId == chatId &&
            Math.Abs(Element<Grid>("SidebarPane").ActualWidth - sidebarWidth) < 1 && NativeUi.IsVisible(Element<Grid>("LogoHost")),
            "empty-conversation search displays zero matches while preserving the home composer, sidebar and draft identity");
        DispatchShortcut(0x1B, control: false);
        await WaitForTranscriptFindAsync(transcript, open: false);
        await WaitAsync(() => !transcript.IsFindTextOpen && !NativeUi.IsVisible(transcript) && Prompt.FocusState != FocusState.Unfocused,
            "Escape closes empty-conversation search and restores the native home input focus");
        var composition = typeof(KYNXA_Desktop.Views.ShellPage).GetField("_promptCompositionActive", PrivateInstance)!;
        composition.SetValue(_shell, true);
        try
        {
            // Exercise the real native guards with a controlled composition state; do not synthesize an operating-system IME.
            // 用受控输入法组合状态执行真实原生守卫，不伪称模拟了操作系统输入法。
            Call("TranscriptSearchShortcut_Invoked", null, null);
            Call("SearchShortcut_Invoked", null, null);
            await SettleAsync();
            Check(!transcript.IsFindTextOpen && !HasPopup<TextBox>("HistorySearchBox") && Prompt.Text == draft && ActiveId == chatId,
                "controlled native composition state blocks both search accelerators without changing the draft");
        }
        finally { composition.SetValue(_shell, false); }
    }

    private async Task CheckBodySearchAfterNativeInputAsync(ConversationTranscript transcript)
    {
        Guid? chatId = ActiveId;
        string draft = Prompt.Text;
        var row = new ConversationMessageViewModel(chatId ?? Guid.NewGuid(), new ChatMessageState
        { Role = "assistant", Content = "KEYBOARD_MATCH first\n\nKEYBOARD_MATCH second\n\nKEYBOARD_MATCH third", Status = "completed" });
        async Task WaitForSearchIndexAsync(int index)
        {
            DateTime deadline = DateTime.UtcNow.AddSeconds(10);
            while (DateTime.UtcNow < deadline)
            {
                using var state = JsonDocument.Parse(await transcript.Browser.ExecuteScriptAsync("kynxaTranscriptSearch.state()"));
                if (state.RootElement.GetProperty("count").GetInt32() == 3
                    && state.RootElement.GetProperty("index").GetInt32() == index) return;
                await Task.Delay(30);
            }
            throw new TimeoutException("Native search did not reach index " + index);
        }
        try
        {
            transcript.ShowConversation(chatId, [row], openAtBottom: false);
            await SettleAsync();
            DispatchShortcut(0x46);
            await WaitForTranscriptFindAsync(transcript, open: true, focused: true);
            await transcript.Browser.ExecuteScriptAsync("(() => {const input=document.getElementById('transcript-search-input');input.value='KEYBOARD_MATCH';input.dispatchEvent(new Event('input',{bubbles:true}));})()");
            await WaitForSearchIndexAsync(-1);
            DispatchShortcut(0x0D, control: false);
            await WaitForSearchIndexAsync(0);
            DispatchShortcut(0x0D, control: false, shift: true);
            await WaitForSearchIndexAsync(2);
            Check(Prompt.Text == draft, "actual Enter and Shift+Enter in search navigate without sending or editing the native draft");

            MovePointerInside(Prompt);
            SendFixtureInput([new NativeInput { Data = new NativeInputData { Mouse = new NativeMouseInput { Flags = 0x0002 } } },
                new NativeInput { Data = new NativeInputData { Mouse = new NativeMouseInput { Flags = 0x0004 } } }]);
            await WaitAsync(() => Prompt.FocusState != FocusState.Unfocused, "actual click moves focus from search into the native composer");
            DispatchShortcut(0x72, control: false);
            await WaitForSearchIndexAsync(0);
            DispatchShortcut(0x72, control: false, shift: true);
            await WaitForSearchIndexAsync(2);
            Check(Prompt.FocusState != FocusState.Unfocused && Prompt.Text == draft && ActiveId == chatId,
                "F3 and Shift+F3 navigate exactly once from the clicked composer while retaining focus, draft and conversation");
            DispatchShortcut(0x46);
            await WaitForTranscriptFindAsync(transcript, open: true, focused: true);
            DispatchShortcut(0x72, control: false);
            await WaitForSearchIndexAsync(0);
            Check(Prompt.Text == draft, "Ctrl+F restores search focus after clicking the composer, and F3 is not handled twice by the host and WebView");
            Prompt.Focus(FocusState.Programmatic);
            DispatchShortcut(0x1B, control: false);
            await WaitForTranscriptFindAsync(transcript, open: false);
            Check(Prompt.FocusState != FocusState.Unfocused && Prompt.Text == draft, "Escape from native input closes search without stealing focus or changing the draft");
        }
        finally
        {
            transcript.CloseFindText();
            transcript.ShowConversation(ActiveId, _shell.ActiveMessages.ToArray(), openAtBottom: false);
        }
    }

    private async Task WaitForTranscriptFindAsync(ConversationTranscript transcript, bool open, bool focused = false)
    {
        DateTime deadline = DateTime.UtcNow.AddSeconds(20);
        while (true)
        {
            using var observation = JsonDocument.Parse(await transcript.Browser.ExecuteScriptAsync("""
                (() => {
                  const state = window.kynxaTranscriptSearch?.state();
                  return { open: !!state?.open,
                    focused: document.hasFocus() && document.activeElement?.id === 'transcript-search-input',
                    documentFocused: document.hasFocus(), activeElement: document.activeElement?.id };
                })()
                """));
            if (observation.RootElement.GetProperty("open").GetBoolean() == open &&
                (!focused || observation.RootElement.GetProperty("focused").GetBoolean())) return;
            if (_unhandled is not null) throw new InvalidOperationException("Unhandled Shell search error", _unhandled);
            if (DateTime.UtcNow >= deadline)
            {
                File.WriteAllText(Path.Combine(_directory, "shell-body-search-focus-timeout.json"), JsonSerializer.Serialize(new
                {
                    Observation = observation.RootElement.Clone(), BrowserFocus = transcript.Browser.FocusState.ToString(),
                    NativeFocus = FocusManager.GetFocusedElement(_shell.XamlRoot)?.GetType().FullName,
                    Foreground = GetForegroundWindow().ToInt64(), Owned = WinRT.Interop.WindowNative.GetWindowHandle(Window).ToInt64()
                }, new JsonSerializerOptions { WriteIndented = true }));
                throw new TimeoutException($"Body search did not reach open={open}, focused={focused}.");
            }
            await Task.Delay(30);
        }
    }

    [DllImport("user32.dll", EntryPoint = "IsWindowVisible")] private static extern bool IsShortcutWindowVisible(nint window);
    [DllImport("user32.dll", EntryPoint = "IsIconic")] private static extern bool IsShortcutWindowIconic(nint window);

    private async Task ActivateOwnedShortcutWindowAsync()
    {
        nint owned = WinRT.Interop.WindowNative.GetWindowHandle(Window);
        if (GetForegroundWindow() == owned) return;
        // A restored window may extend beyond the monitor before the hover checks resize it.
        // 悬停检查调整尺寸之前，恢复后的窗口可能超出屏幕；激活坐标必须先落在可达区域。
        var workArea = DisplayArea.GetFromWindowId(Window.AppWindow.Id, DisplayAreaFallback.Primary).WorkArea;
        var currentSize = Window.AppWindow.Size;
        var currentPosition = Window.AppWindow.Position;
        int reachableWidth = Math.Min(currentSize.Width, workArea.Width - 32);
        int reachableHeight = Math.Min(currentSize.Height, workArea.Height - 32);
        if (currentPosition.X < workArea.X || currentPosition.Y < workArea.Y ||
            currentPosition.X + currentSize.Width > workArea.X + workArea.Width ||
            currentPosition.Y + currentSize.Height > workArea.Y + workArea.Height)
        {
            Window.AppWindow.MoveAndResize(new Windows.Graphics.RectInt32(workArea.X + 16, workArea.Y + 16,
                reachableWidth, reachableHeight));
        }
        Window.AppWindow.Show(activateWindow: true);
        Window.Activate();
        SetForegroundWindow(owned);
        Window.AppWindow.MoveInZOrderAtTop();
        await SettleAsync();
        if (GetForegroundWindow() == owned) return;
        // Windows can reject background activation. Click only the verified blank title bar of this visible fixture.
        // Windows 可能拒绝后台激活；仅点击已核验可见、属于本夹具的标题栏空白位置。
        if (!IsShortcutWindowVisible(owned) || IsShortcutWindowIconic(owned) || !GetCursorPos(out var originalPointer))
            throw new InvalidOperationException("The owned shortcut fixture is not available for native activation.");
        foreach (int key in new[] { 0x01, 0x02, 0x04, 0x10, 0x11, 0x12, 0x5B, 0x5C })
            if ((GetAsyncKeyState(key) & 0x8000) != 0)
                throw new InvalidOperationException("Fixture activation stopped because a mouse button or modifier is held.");
        var presenter = Window.AppWindow.Presenter as OverlappedPresenter
            ?? throw new InvalidOperationException("The shortcut fixture has no overlapped presenter.");
        bool wasAlwaysOnTop = presenter.IsAlwaysOnTop;
        try
        {
            // The desktop can cover a background test with an always-on-top window. Raise only this owned fixture while activating it.
            // 桌面置顶窗口可能遮挡后台夹具；激活期间仅临时提升本夹具，并在 finally 恢复原设置。
            presenter.IsAlwaysOnTop = true;
            Window.AppWindow.MoveInZOrderAtTop();
            await SettleAsync();
            double scale = _shell.XamlRoot.RasterizationScale;
            var point = new NativePoint { X = (int)Math.Round(((FrameworkElement)Window.Content).ActualWidth * 0.6 * scale), Y = (int)Math.Round(24 * scale) };
            if (!ClientToScreen(owned, ref point) || GetAncestor(WindowFromPoint(point), 2) != owned)
                throw new InvalidOperationException("The fixture's blank title bar is covered or outside the reachable screen.");
            int left = GetSystemMetrics(76), top = GetSystemMetrics(77), width = GetSystemMetrics(78), height = GetSystemMetrics(79);
            if (width <= 1 || height <= 1 || point.X < left || point.Y < top || point.X >= left + width || point.Y >= top + height)
                throw new InvalidOperationException("The fixture activation target is outside the virtual screen.");
            SendFixtureInput([new NativeInput { Data = new NativeInputData { Mouse = new NativeMouseInput
            {
                X = (int)Math.Round((point.X - left) * 65535d / (width - 1)),
                Y = (int)Math.Round((point.Y - top) * 65535d / (height - 1)), Flags = 0x8000 | 0x4000 | 0x0001
            } } }]);
            await Task.Delay(40);
            if (!GetCursorPos(out var actual) || Math.Abs(actual.X - point.X) > 1 || Math.Abs(actual.Y - point.Y) > 1 ||
                GetAncestor(WindowFromPoint(actual), 2) != owned)
                throw new InvalidOperationException("The owned fixture activation point changed before clicking.");
            SendFixtureInput([new NativeInput { Data = new NativeInputData { Mouse = new NativeMouseInput { Flags = 0x0002 } } },
                new NativeInput { Data = new NativeInputData { Mouse = new NativeMouseInput { Flags = 0x0004 } } }]);
            await WaitAsync(() => GetForegroundWindow() == owned, "clicking the owned blank title bar activates only this fixture");
        }
        finally
        {
            presenter.IsAlwaysOnTop = wasAlwaysOnTop;
            if (GetForegroundWindow() == owned) SetCursorPos(originalPointer.X, originalPointer.Y);
        }
    }

    private sealed record ShortcutScrollObservation(double Top, double Height, double Viewport, bool HasFixture);

    private async Task<bool> CheckScrolledTranscriptTooltipAsync(ConversationTranscript transcript, Guid chatId)
    {
        var originalRows = _shell.ActiveMessages.ToArray();
        var ownedMessage = new ChatMessageState
        {
            Role = "assistant",
            Content = string.Join("\n\n", Enumerable.Range(1, 160).Select(index =>
                $"SCROLL_TOOLTIP_FIXTURE_{index:000} 这是仅供原生滚动验收的虚构正文，不写入正式会话。"))
        };
        var ownedRow = new ConversationMessageViewModel(chatId, ownedMessage);
        async Task<ShortcutScrollObservation> ObserveAsync()
        {
            string script = "(() => { const s = document.scrollingElement; return { Top: s.scrollTop, " +
                "Height: s.scrollHeight, Viewport: s.clientHeight, HasFixture: !!document.querySelector(" +
                JsonSerializer.Serialize($"article[data-message-id=\"{ownedMessage.Id}\"]") + ") }; })()";
            return JsonSerializer.Deserialize<ShortcutScrollObservation>(await transcript.Browser.ExecuteScriptAsync(script))
                ?? throw new InvalidOperationException("The isolated transcript returned no scroll observation.");
        }
        try
        {
            // The synthetic row belongs only to this view; the Shell collection and formal messages stay untouched.
            // 虚构行仅由本次视图持有，不加入 Shell 集合，也不修改正式消息。
            transcript.ShowConversation(chatId, [.. originalRows, ownedRow], openAtBottom: true);
            DateTime deadline = DateTime.UtcNow.AddSeconds(15);
            ShortcutScrollObservation before;
            do
            {
                if (DateTime.UtcNow >= deadline) throw new TimeoutException("The isolated long transcript did not finish rendering at the bottom.");
                await Task.Delay(30);
                before = await ObserveAsync();
            } while (!before.HasFixture || before.Viewport <= 0 || before.Height < before.Viewport * 3 ||
                before.Top < before.Height - before.Viewport - 2);
            Check(before.Height > before.Viewport * 3 && before.Top > 20,
                "the real production renderer lays out an isolated long reply with a scrollable document");

            bool sawBareBeforeEnteringTranscript = HasBareShortcutTooltip();
            bool sawBareDuringScroll = sawBareBeforeEnteringTranscript;
            MovePointerInside(transcript);
            for (int index = 0; index < 3; index++)
            {
                // Positive Windows wheel delta scrolls upward from the proven bottom position.
                // Windows 滚轮正值从已确认的底部向上滚动。
                DispatchPointerWheel(120);
                await Task.Delay(100);
                sawBareDuringScroll |= HasBareShortcutTooltip();
            }
            await Task.Delay(250);
            var after = await ObserveAsync();
            bool sawBareAfterWheels = HasBareShortcutTooltip();
            await NativeWindowCapture.CaptureAsync(Window, Path.Combine(_directory, "shell-shortcut-tooltip-after-scroll.png"));
            bool? sawBareAfterContinuedHover = null;
            void WriteScrollObservation() => File.WriteAllText(Path.Combine(_directory, "shell-shortcut-tooltip-scroll.json"),
                JsonSerializer.Serialize(new
                {
                    Before = before, After = after, ScrolledPixels = before.Top - after.Top,
                    SawBareBeforeEnteringTranscript = sawBareBeforeEnteringTranscript,
                    SawBareDuringScroll = sawBareDuringScroll,
                    SawBareAfterWheels = sawBareAfterWheels,
                    SawBareAfterContinuedHover = sawBareAfterContinuedHover
                }, new JsonSerializerOptions { WriteIndented = true }));
            // Preserve the observed scroll and tooltip state before asserting, including a failed native-wheel run.
            // 断言前保留实际滚动与提示状态，原生滚轮验收失败时也能回看现场。
            WriteScrollObservation();
            Check(after.HasFixture && before.Top - after.Top > 20,
                $"native wheel input actually scrolls the long transcript upward ({before.Top:F1} to {after.Top:F1} pixels)");
            await Task.Delay(1100);
            sawBareAfterContinuedHover = HasBareShortcutTooltip();
            WriteScrollObservation();
            return sawBareDuringScroll || sawBareAfterWheels || sawBareAfterContinuedHover == true;
        }
        finally
        {
            // Restore the production's current source, dropping only the fixture-owned view row even after failure.
            // 即使验收失败，也恢复生产当前消息来源，仅移除夹具拥有的临时视图行。
            transcript.ShowConversation(ActiveId, _shell.ActiveMessages.ToArray(), openAtBottom: false);
            await SettleAsync();
        }
    }

    private void RequireFixtureForeground()
    {
        if (GetForegroundWindow() != WinRT.Interop.WindowNative.GetWindowHandle(Window))
            throw new InvalidOperationException("Native test input stopped because the fixture window is no longer foreground.");
    }

    private void MovePointerInside(FrameworkElement element, double widthFraction = 0.5, double heightFraction = 0.5)
    {
        RequireFixtureForeground();
        if (!NativeUi.IsVisible(element)) throw new InvalidOperationException("Cannot hover an invisible fixture control: " + element.Name);
        Point point = element.TransformToVisual((FrameworkElement)Window.Content)
            .TransformPoint(new Point(element.ActualWidth * widthFraction, element.ActualHeight * heightFraction));
        double scale = _shell.XamlRoot.RasterizationScale;
        var nativePoint = new NativePoint { X = (int)Math.Round(point.X * scale), Y = (int)Math.Round(point.Y * scale) };
        nint ownedWindow = WinRT.Interop.WindowNative.GetWindowHandle(Window);
        if (!ClientToScreen(ownedWindow, ref nativePoint)) throw new InvalidOperationException("Cannot locate the fixture pointer target on screen.");
        int virtualLeft = GetSystemMetrics(76), virtualTop = GetSystemMetrics(77);
        int virtualWidth = GetSystemMetrics(78), virtualHeight = GetSystemMetrics(79);
        if (virtualWidth <= 1 || virtualHeight <= 1 || nativePoint.X < virtualLeft || nativePoint.Y < virtualTop ||
            nativePoint.X >= virtualLeft + virtualWidth || nativePoint.Y >= virtualTop + virtualHeight ||
            GetAncestor(WindowFromPoint(nativePoint), 2) != ownedWindow)
            throw new InvalidOperationException("The native pointer target is outside the fixture's reachable screen area.");
        int absoluteX = (int)Math.Round((nativePoint.X - virtualLeft) * 65535d / (virtualWidth - 1));
        int absoluteY = (int)Math.Round((nativePoint.Y - virtualTop) * 65535d / (virtualHeight - 1));
        // Inject an actual mouse move so XAML receives PointerEntered; merely relocating the cursor is insufficient.
        // 注入真实鼠标移动，确保 XAML 收到 PointerEntered；仅调整光标坐标不足以触发悬停。
        RequireFixtureForeground();
        SendFixtureInput([new NativeInput { Data = new NativeInputData { Mouse = new NativeMouseInput
            { X = absoluteX, Y = absoluteY, Flags = 0x8000 | 0x4000 | 0x0001 } } }]);
        if (!GetCursorPos(out var actualPoint)) throw new InvalidOperationException("Cannot verify the native fixture pointer position.");
        nint hitWindow = WindowFromPoint(actualPoint);
        nint hitRoot = GetAncestor(hitWindow, 2);
        _shortcutPointerObservations.Add(new
        {
            Element = element.Name,
            PointDips = new { point.X, point.Y },
            RasterizationScale = scale,
            InputMethod = "SendInput ABSOLUTE VIRTUALDESK MOVE",
            VirtualScreen = new { Left = virtualLeft, Top = virtualTop, Width = virtualWidth, Height = virtualHeight },
            NormalizedPoint = new { X = absoluteX, Y = absoluteY },
            ExpectedPoint = new { nativePoint.X, nativePoint.Y },
            ActualPoint = new { actualPoint.X, actualPoint.Y },
            HitWindow = hitWindow.ToInt64(), HitRoot = hitRoot.ToInt64(), OwnedWindow = ownedWindow.ToInt64()
        });
        File.WriteAllText(Path.Combine(_directory, "shell-shortcut-tooltip-pointer.json"),
            JsonSerializer.Serialize(_shortcutPointerObservations, new JsonSerializerOptions { WriteIndented = true }));
        if (Math.Abs(actualPoint.X - nativePoint.X) > 1 || Math.Abs(actualPoint.Y - nativePoint.Y) > 1 || hitRoot != ownedWindow)
            throw new InvalidOperationException($"The requested fixture hover point is clipped or belongs to another window: " +
                $"expected ({nativePoint.X},{nativePoint.Y}), actual ({actualPoint.X},{actualPoint.Y}), root {hitRoot}, owned {ownedWindow}.");
    }

    private void DispatchShortcut(ushort virtualKey, bool control = true, bool shift = false)
    {
        RequireFixtureForeground();
        foreach (int modifier in new[] { 0x10, 0x11, 0x12, 0x5B, 0x5C })
            if ((GetAsyncKeyState(modifier) & 0x8000) != 0)
                throw new InvalidOperationException("Native shortcut input stopped because a modifier key is already held.");
        NativeInput Key(ushort key, bool up) => new() { Type = 1,
            Data = new NativeInputData { Keyboard = new NativeKeyboardInput { VirtualKey = key, Flags = up ? 2u : 0u } } };
        var inputs = new List<NativeInput>();
        if (control) inputs.Add(Key(0x11, false));
        if (shift) inputs.Add(Key(0x10, false));
        inputs.Add(Key(virtualKey, false));
        inputs.Add(Key(virtualKey, true));
        if (shift) inputs.Add(Key(0x10, true));
        if (control) inputs.Add(Key(0x11, true));
        SendFixtureInput(inputs.ToArray());
    }

    private void DispatchPointerWheel(int wheelDelta)
    {
        RequireFixtureForeground();
        SendFixtureInput([new NativeInput { Data = new NativeInputData { Mouse = new NativeMouseInput
            { MouseData = unchecked((uint)wheelDelta), Flags = 0x0800 } } }]);
    }

    private static void SendFixtureInput(NativeInput[] inputs)
    {
        uint sent = SendInput((uint)inputs.Length, inputs, Marshal.SizeOf<NativeInput>());
        if (sent != inputs.Length) throw new InvalidOperationException($"Native fixture input sent {sent}/{inputs.Length} events; Win32 error {Marshal.GetLastWin32Error()}.");
    }

    [StructLayout(LayoutKind.Sequential)] private struct NativePoint { public int X; public int Y; }
    [StructLayout(LayoutKind.Sequential)] private struct NativeInput { public uint Type; public NativeInputData Data; }
    [StructLayout(LayoutKind.Explicit)] private struct NativeInputData
    {
        [FieldOffset(0)] public NativeMouseInput Mouse;
        [FieldOffset(0)] public NativeKeyboardInput Keyboard;
    }
    [StructLayout(LayoutKind.Sequential)] private struct NativeMouseInput
    {
        public int X; public int Y; public uint MouseData; public uint Flags; public uint Time; public nuint ExtraInfo;
    }
    [StructLayout(LayoutKind.Sequential)] private struct NativeKeyboardInput
    {
        public ushort VirtualKey; public ushort ScanCode; public uint Flags; public uint Time; public nuint ExtraInfo;
    }
    [DllImport("user32.dll")] private static extern nint GetForegroundWindow();
    [DllImport("user32.dll")] private static extern short GetAsyncKeyState(int virtualKey);
    [DllImport("user32.dll", SetLastError = true)] private static extern uint SendInput(uint count, NativeInput[] inputs, int size);
    [DllImport("user32.dll")] private static extern bool GetCursorPos(out NativePoint point);
    [DllImport("user32.dll")] private static extern bool SetCursorPos(int x, int y);
    [DllImport("user32.dll")] private static extern bool ClientToScreen(nint windowHandle, ref NativePoint point);
    [DllImport("user32.dll")] private static extern nint WindowFromPoint(NativePoint point);
    [DllImport("user32.dll")] private static extern nint GetAncestor(nint windowHandle, uint flags);
    [DllImport("user32.dll")] private static extern int GetSystemMetrics(int index);
}

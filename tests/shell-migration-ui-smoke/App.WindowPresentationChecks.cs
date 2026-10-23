using System.Collections.Concurrent;
using System.Diagnostics;
using System.Reflection;
using System.Runtime.InteropServices;
using System.Text.Json;
using System.Text;
using KYNXA_Desktop.Models.UI;
using KYNXA_Desktop.Services;
using MemoryUiSmoke;
using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Automation;
using Microsoft.UI.Xaml.Automation.Peers;
using Microsoft.UI.Xaml.Automation.Provider;
using Microsoft.UI.Xaml.Controls;
using Microsoft.UI.Xaml.Controls.Primitives;
using Microsoft.UI.Xaml.Media;
using Microsoft.UI.Windowing;
using Windows.Foundation;

namespace KYNXA_Desktop;

public partial class App
{
    private sealed record PresentationFrame(string Case, string Phase, string Target, string Source,
        long Milliseconds, uint NativeEvent, uint EventTime, uint DeliveryDelayMs, long Handle,
        bool Visible, int Width, int Height, int ScreenX, int ScreenY, bool OwnedPixel, string? Pixel,
        long HitRoot, long Foreground, uint ForegroundProcess, string ForegroundClass, string HitClass,
        long? SampleStartedTicks = null, long? DismissedTicksAtSample = null, long? SampleCompletedTicks = null);

    private readonly ConcurrentQueue<PresentationFrame> _presentationFrames = new();
    private string _presentationCase = string.Empty;
    private string _presentationPhase = string.Empty;
    private readonly Stopwatch _presentationClock = new();
    private Exception? _presentationHookError;
    private sealed record PresentationEntry(Action Invoke, Window Owner, Popup? MenuPopup = null, FlyoutBase? PickerFlyout = null,
        Popup? SourcePopup = null, MenuFlyoutPresenter? MenuPresenter = null, Func<Task>? AfterInvoke = null);
    private sealed record OpeningObservation(string Case, string State, string DismissBoundary, long Handle,
        long TriggerTicks, long? DismissedTicks, long FirstVisibleTicks, long? FirstOwnedPixelTicks,
        bool VisibleBeforeDismissed, bool OwnedPixelBeforeDismissed, int Polls, long MaximumPollGapTicks, long? PopupClosedTicks);
    private readonly ConcurrentQueue<OpeningObservation> _presentationOpenings = new();
    private sealed record EraseContract(string Case, string Mode, string ExpectedMain, string Target, long Handle,
        uint ExpectedColorRef, uint? ActualColorRef, string? ActualColor, long NativeResult, bool FellBackToWhite, string? Error);
    private readonly ConcurrentQueue<EraseContract> _presentationEraseContracts = new();
    private readonly List<(nint Handle, PresentationEraseSubclass Callback)> _presentationEraseHooks = [];
    private static readonly nuint PresentationEraseSubclassId = 0x4B595850;

    private void AttachEraseContractProbe(Window child)
    {
        nint handle = WinRT.Interop.WindowNative.GetWindowHandle(child);
        string caseName = _presentationCase, mode = AppearanceService.Mode, expected = AppearanceService.Current.Main;
        string target = child.GetType().Name;
        var color = AppearanceService.ParseColor(expected);
        uint expectedColorRef = (uint)(color.R | color.G << 8 | color.B << 16);
        bool observed = false;
        PresentationEraseSubclass? callback = null;
        callback = (window, message, wParam, lParam, subclassId, _) =>
        {
            if (message == 0x0002 && !observed)
            {
                observed = true;
                try
                {
                    // Real WM_DESTROY follows SDK content removal. Call below this observer with an offscreen DC, without reentry.
                    // 真实 WM_DESTROY 位于 SDK 清除内容之后；用离屏 DC 向当前观察器下方调用，不重入自身或绘制显示面。
                    var result = ProbeTeardownErase(window);
                    uint actual = result.Color;
                    _presentationEraseContracts.Enqueue(new(caseName, mode, expected, target, window.ToInt64(), expectedColorRef,
                        actual, $"#{actual & 255:X2}{(actual >> 8) & 255:X2}{(actual >> 16) & 255:X2}", result.Result,
                        actual == 0x00FFFFFF, null));
                }
                catch (Exception error)
                {
                    _presentationEraseContracts.Enqueue(new(caseName, mode, expected, target, window.ToInt64(), expectedColorRef,
                        null, null, 0, false, error.ToString()));
                }
            }
            if (message == 0x0082) PresentationRemoveSubclass(window, callback!, subclassId);
            return PresentationDefSubclass(window, message, wParam, lParam);
        };
        if (!PresentationSetSubclass(handle, callback, PresentationEraseSubclassId, 0))
            throw new InvalidOperationException("Cannot attach the owned teardown erase-contract observer.");
        _presentationEraseHooks.Add((handle, callback));
    }

    private static (long Result, uint Color) ProbeTeardownErase(nint window)
    {
        nint source = PresentationGetDC(0), memory = 0, bitmap = 0, previous = 0, sentinel = 0;
        try
        {
            if (source == 0 || (memory = PresentationCreateCompatibleDC(source)) == 0 ||
                (bitmap = PresentationCreateCompatibleBitmap(source, 8, 8)) == 0)
                throw new InvalidOperationException("Cannot allocate the task-owned 8 by 8 erase-contract bitmap.");
            previous = PresentationSelectObject(memory, bitmap);
            if (previous == 0 || previous == -1) throw new InvalidOperationException("Cannot select the erase-contract bitmap.");
            sentinel = PresentationCreateSolidBrush(0x00FF00FF);
            var rectangle = new PresentationRect { Right = 8, Bottom = 8 };
            if (sentinel == 0 || PresentationFillRect(memory, ref rectangle, sentinel) == 0)
                throw new InvalidOperationException("Cannot initialize the offscreen erase-contract sentinel.");
            nint result = PresentationDefSubclass(window, 0x0014, memory, 0);
            uint actual = PresentationGetPixel(memory, 0, 0);
            if (actual == uint.MaxValue) throw new InvalidOperationException("Cannot read the offscreen native erase result.");
            return (result.ToInt64(), actual);
        }
        finally
        {
            if (sentinel != 0) PresentationDeleteObject(sentinel);
            if (previous != 0 && previous != -1) PresentationSelectObject(memory, previous);
            if (bitmap != 0) PresentationDeleteObject(bitmap);
            if (memory != 0) PresentationDeleteDC(memory);
            if (source != 0) PresentationReleaseDC(0, source);
        }
    }

    private static Window[] PresentationTrackedWindows() =>
        ((IEnumerable<Window>)typeof(AppearanceService).GetField("TrackedWindows", BindingFlags.Static | BindingFlags.NonPublic)!
            .GetValue(null)!).ToArray();

    private static PresentationPoint PresentationClientProbe(PresentationRect client, bool isSettingsWindow) => new()
    {
        // Settings has a right-hand ScrollBar with white arrow glyphs; its left content gutter stays blank at every scroll offset.
        // 设置右侧滚动条含白色箭头；左侧内容留白在各滚动位置均为空，避免把合法字形误判为页面背景闪白。
        X = isSettingsWindow ? 12 : client.Right - 12,
        Y = client.Bottom - 12
    };

    private PresentationFrame? RecordPresentationFrame(Window window, string source,
        uint nativeEvent = 0, uint eventTime = 0, PresentationPoint? screenPoint = null, string? target = null)
    {
        if (_presentationCase.Length == 0) return null;
        nint handle = WinRT.Interop.WindowNative.GetWindowHandle(window);
        // Never read a destroyed or hidden child's display surface; its former area belongs to the owner probe.
        // 不读取已销毁或隐藏的附属窗口显示面；其原位置交由所有者窗口探测。
        if (!PresentationIsWindow(handle) || !PresentationIsVisible(handle)) return null;
        if (!PresentationClientRect(handle, out var client) || client.Right < 25 || client.Bottom < 25) return null;
        var point = screenPoint ?? PresentationClientProbe(client, window.GetType() == typeof(Window));
        if (screenPoint is null && !PresentationClientToScreen(handle, ref point)) return null;
        nint hitRoot = PresentationRootAtPoint(point);
        bool owned = hitRoot == handle;
        string? pixel = null;
        if (owned)
        {
            // Read one actual composed desktop pixel only after verifying the point belongs to this exact HWND.
            // 先核验该屏幕点确属当前 HWND，再读取一个真实桌面合成像素，不读取其他窗口内容。
            nint dc = PresentationGetDC(0);
            try
            {
                uint color = PresentationGetPixel(dc, point.X, point.Y);
                if (color != uint.MaxValue) pixel = $"#{color & 255:X2}{(color >> 8) & 255:X2}{(color >> 16) & 255:X2}";
            }
            finally { PresentationReleaseDC(0, dc); }
        }
        var size = window.AppWindow.Size;
        nint foreground = GetForegroundWindow();
        PresentationWindowProcess(foreground, out uint foregroundProcess);
        var frame = new PresentationFrame(_presentationCase, _presentationPhase,
            target ?? (ReferenceEquals(window, Window) ? "owner" : window.GetType().Name), source,
            _presentationClock.ElapsedMilliseconds, nativeEvent, eventTime,
            eventTime == 0 ? 0 : unchecked((uint)Environment.TickCount - eventTime), handle.ToInt64(),
            true, size.Width, size.Height, point.X, point.Y, owned, pixel,
            hitRoot.ToInt64(), foreground.ToInt64(), foregroundProcess, PresentationClass(foreground), PresentationClass(hitRoot));
        _presentationFrames.Enqueue(frame);
        return frame;
    }

    private void RecordClosingNativeFrame(nint handle, string target, PresentationPoint point, string caseName,
        string phase = "closing", string source = "close-native-poll", long? dismissedTicksAtSample = null)
    {
        long sampleStartedTicks = Stopwatch.GetTimestamp();
        if (!PresentationIsWindow(handle) || !PresentationIsVisible(handle) || !PresentationWindowRect(handle, out var bounds)) return;
        nint hitRoot = PresentationRootAtPoint(point);
        bool owned = hitRoot == handle;
        string? pixel = null;
        if (owned)
        {
            nint dc = PresentationGetDC(0);
            try
            {
                uint color = PresentationGetPixel(dc, point.X, point.Y);
                if (color != uint.MaxValue) pixel = $"#{color & 255:X2}{(color >> 8) & 255:X2}{(color >> 16) & 255:X2}";
            }
            finally { PresentationReleaseDC(0, dc); }
        }
        nint foreground = GetForegroundWindow();
        PresentationWindowProcess(foreground, out uint process);
        _presentationFrames.Enqueue(new(caseName, phase, target, source, _presentationClock.ElapsedMilliseconds,
            0, 0, 0, handle.ToInt64(), true, bounds.Right - bounds.Left, bounds.Bottom - bounds.Top,
            point.X, point.Y, owned, pixel, hitRoot.ToInt64(), foreground.ToInt64(), process,
            PresentationClass(foreground), PresentationClass(hitRoot), sampleStartedTicks, dismissedTicksAtSample, Stopwatch.GetTimestamp()));
    }

    private async Task TraceNativeCloseAsync(Window child, PresentationPoint ownerPoint, Window? owner = null)
    {
        nint childHandle = WinRT.Interop.WindowNative.GetWindowHandle(child);
        nint ownerHandle = WinRT.Interop.WindowNative.GetWindowHandle(owner ?? Window);
        PresentationClientRect(childHandle, out var client);
        var childPoint = PresentationClientProbe(client, child.GetType() == typeof(Window));
        PresentationClientToScreen(childHandle, ref childPoint);
        string target = child.GetType().Name, caseName = _presentationCase;
        var started = new TaskCompletionSource(TaskCreationOptions.RunContinuationsAsynchronously);
        // Captured handles and coordinates let sampling continue during synchronous SDK teardown.
        // 采样仅依赖已捕获句柄和坐标，SDK 同步关闭阻塞界面时仍能观察合成帧。
        var sampling = Task.Run(async () =>
        {
            var elapsed = Stopwatch.StartNew();
            started.TrySetResult();
            while (elapsed.ElapsedMilliseconds < 500)
            {
                RecordClosingNativeFrame(childHandle, target, childPoint, caseName);
                RecordClosingNativeFrame(ownerHandle, "owner", ownerPoint, caseName);
                await Task.Delay(2).ConfigureAwait(false);
            }
        });
        await started.Task;
        NativeUi.RequestClose(child);
        await sampling;
    }

    private static string PresentationClass(nint handle)
    {
        var name = new StringBuilder(128);
        PresentationClassName(handle, name, name.Capacity);
        return name.ToString();
    }

    private static PresentationPoint? FormerChildPoint(Window child, Window owner)
    {
        nint childHandle = WinRT.Interop.WindowNative.GetWindowHandle(child);
        nint ownerHandle = WinRT.Interop.WindowNative.GetWindowHandle(owner);
        if (!PresentationClientRect(childHandle, out var childRect) || !PresentationClientRect(ownerHandle, out var ownerRect)) return null;
        var childOrigin = new PresentationPoint();
        var ownerOrigin = new PresentationPoint();
        if (!PresentationClientToScreen(childHandle, ref childOrigin) || !PresentationClientToScreen(ownerHandle, ref ownerOrigin)) return null;
        int left = Math.Max(childOrigin.X, ownerOrigin.X), top = Math.Max(childOrigin.Y, ownerOrigin.Y);
        int right = Math.Min(childOrigin.X + childRect.Right, ownerOrigin.X + ownerRect.Right);
        int bottom = Math.Min(childOrigin.Y + childRect.Bottom, ownerOrigin.Y + ownerRect.Bottom);
        return right - left > 30 && bottom - top > 30 ? new PresentationPoint { X = right - 12, Y = bottom - 12 } : null;
    }

    private async Task TracePresentationForAsync(Window child, TimeSpan duration, PresentationPoint? ownerPoint = null,
        Func<bool>? childClosed = null, Window? owner = null)
    {
        var elapsed = Stopwatch.StartNew();
        while (elapsed.Elapsed < duration)
        {
            if (childClosed?.Invoke() != true) RecordPresentationFrame(child, "display-poll");
            RecordPresentationFrame(owner ?? Window, "display-poll", screenPoint: ownerPoint, target: "owner");
            await Task.Delay(8);
        }
    }

    private static bool PresentationWhite(string color) =>
        Convert.ToInt32(color.Substring(1, 2), 16) >= 240 &&
        Convert.ToInt32(color.Substring(3, 2), 16) >= 240 &&
        Convert.ToInt32(color.Substring(5, 2), 16) >= 240;

    private static string PresentationChildField(string route) => route.StartsWith("model-", StringComparison.Ordinal)
        ? "_modelManagementWindow" : "_storageSettingsWindow";

    private Button PresentationSidebarButton(string key) => NativeUi.Descendants<Button>(_shell).Single(button =>
        NativeUi.IsVisible(button) && (AutomationProperties.GetName(button) == UiText.Get(key) ||
            NativeUi.Descendants<TextBlock>(button).Any(text => text.Text == UiText.Get(key))));

    private async Task<PresentationEntry> PreparePresentationEntryAsync(string route)
    {
        await ActivateOwnedShortcutWindowAsync();
        bool drawer = route.EndsWith("-drawer", StringComparison.Ordinal), rail = route == "settings-rail";
        ResizeInDips(drawer ? 800 : 1440, drawer ? 720 : 900);
        await WaitAsync(() => ShellField<bool>("_isCompactLayout") == drawer &&
            (drawer ? Element<Grid>("ShellGrid").ActualWidth is > 0 and < KYNXA_Desktop.Layout.ShellLayoutMetrics.CompactSidebarBreakpoint
                : Element<Grid>("ShellGrid").ActualWidth >= KYNXA_Desktop.Layout.ShellLayoutMetrics.CompactSidebarBreakpoint),
            route + " reaches its actual " + (drawer ? "compact" : "wide") + " Shell layout");
        await SettleAsync();
        var expand = Element<Button>("CompactSidebarButton");
        var collapse = Element<Button>("CollapseSidebarButton");
        if (rail && NativeUi.IsVisible(collapse)) NativeUi.Invoke(collapse);
        else if (!rail && NativeUi.IsVisible(expand)) NativeUi.Invoke(expand);
        await WaitAsync(() => rail ? NativeUi.IsVisible(expand) && !NativeUi.IsVisible(collapse) :
            NativeUi.IsVisible(collapse) && !NativeUi.IsVisible(expand) && (!drawer || NativeUi.IsVisible(Element<Button>("SidebarScrim"))),
            route + " presents its actual " + (rail ? "collapsed rail" : drawer ? "temporary drawer" : "expanded sidebar") + " controls");
        if (drawer || rail)
        {
            Check(NativeUi.IsVisible(Element<Button>("SidebarScrim")) == drawer,
                route + " exercises the requested drawer or rail without forcing an expanded wide layout");
        }
        await SettleAsync();
        if (route.EndsWith("-more", StringComparison.Ordinal))
        {
            string key = route.StartsWith("model-", StringComparison.Ordinal) ? "模型管理" :
                route.StartsWith("tools-", StringComparison.Ordinal) ? "工具与技能" : "设置";
            var more = NativeUi.Descendants<Button>((FrameworkElement)Window.Content)
                .Single(button => AutomationProperties.GetName(button) == UiText.Get("更多功能"));
            NativeUi.Invoke(more);
            await WaitAsync(() => OpenNativePopups().Any(popup => NativeUi.Descendants<MenuFlyoutItem>(popup.Child!)
                .Any(item => item.Text == UiText.Get(key))), route + " opens the actual MainWindow navigation menu");
            var popup = OpenNativePopups().Single(candidate => NativeUi.Descendants<MenuFlyoutItem>(candidate.Child!)
                .Any(item => item.Text == UiText.Get(key)));
            await WaitAsync(() => popup.Child is FrameworkElement { IsLoaded: true, ActualWidth: > 25, ActualHeight: > 25 },
                route + " lays out its original menu presenter before the source point is captured");
            await SettleAsync();
            var item = NativeUi.Descendants<MenuFlyoutItem>(popup.Child!).Single(item => item.Text == UiText.Get(key));
            var peer = FrameworkElementAutomationPeer.CreatePeerForElement(item);
            if (peer?.GetPattern(PatternInterface.Invoke) is not IInvokeProvider invoke)
                throw new InvalidOperationException("The production navigation item does not expose native Invoke automation.");
            var menuPresenter = NativeUi.Descendants<MenuFlyoutPresenter>(popup.Child!).Single(presenter =>
                NativeUi.Descendants<MenuFlyoutItem>(presenter).Any(candidate => ReferenceEquals(candidate, item)));
            return new(() => invoke.Invoke(), Window, MenuPopup: popup, SourcePopup: popup, MenuPresenter: menuPresenter);
        }
        if (route is "model-picker" or "model-status")
        {
            NativeUi.Invoke(Element<Button>(route == "model-picker" ? "ModelPickerButton" : "ModelStatusButton"));
            await WaitAsync(() => HasPopup<Button>("ConfigureCustomModelsButton"), route + " opens its actual model picker");
            var configure = Popup<Button>("ConfigureCustomModelsButton");
            var popup = OpenNativePopups().Single(candidate => candidate.Child is not null &&
                NativeUi.Descendants<Button>(candidate.Child).Any(button => ReferenceEquals(button, configure)));
            await WaitAsync(() => popup.Child is FrameworkElement { IsLoaded: true, ActualWidth: > 25, ActualHeight: > 25 },
                route + " lays out its original picker presenter before the source point is captured");
            await SettleAsync();
            return new(() => NativeUi.Invoke(configure), Window, PickerFlyout: ShellField<FlyoutBase>("_modelPickerFlyout"), SourcePopup: popup);
        }
        if (route == "tools-settings")
        {
            // Arm the native sampler before opening settings, then navigate inside that same visible HWND.
            // 原生采样先就绪再打开设置；工具分类在同一可见 HWND 中切换，不再期待新的工具窗口。
            var openSettings = PresentationSidebarButton("设置");
            return new(() => NativeUi.Invoke(openSettings), Window, AfterInvoke: async () =>
            {
                await WaitAsync(() => ShellField<Window?>("_storageSettingsWindow")?.Content is FrameworkElement { IsLoaded: true },
                    "the actual settings entry realizes the tools category");
                var settings = ShellField<Window>("_storageSettingsWindow");
                var button = NativeUi.ById<Button>((FrameworkElement)settings.Content, "SettingsCategory_tools");
                button.StartBringIntoView();
                await SettleAsync();
                NativeUi.Invoke(button);
            });
        }
        string sidebarKey = route switch
        {
            "model-sidebar" or "model-drawer" => "模型管理", "tools-skills" or "tools-drawer" => "技能",
            "settings-sidebar" or "settings-drawer" or "settings-rail" => "设置", _ => "知识库"
        };
        var sidebarButton = PresentationSidebarButton(sidebarKey);
        return new(() => NativeUi.Invoke(sidebarButton), Window);
    }

    private static nint[] PresentationThreadWindows(uint guiThread, uint ownProcess, string nativeClass)
    {
        var handles = new List<nint>();
        PresentationEnumWindow callback = (handle, _) =>
        {
            uint thread = PresentationWindowProcess(handle, out uint process);
            if (thread == guiThread && process == ownProcess && PresentationGetAncestor(handle, 2) == handle &&
                PresentationClass(handle) == nativeClass) handles.Add(handle);
            return true;
        };
        PresentationEnumThreadWindows(guiThread, callback, 0);
        GC.KeepAlive(callback);
        return handles.ToArray();
    }

    private PresentationFrame? RecordOpeningNativeFrame(nint handle, string target, string caseName, string phase, ref long dismissedTicks,
        PresentationPoint? screenPoint = null)
    {
        // Snapshot ordering before native reads; Closed during GetPixel must not reclassify the sample.
        // 原生查询和读像素前固定时序，GetPixel 期间的 Closed 不改变样本归属。
        long sampleStartedTicks = Stopwatch.GetTimestamp(), dismissedTicksAtSample = Volatile.Read(ref dismissedTicks);
        if (!PresentationIsWindow(handle) || !PresentationIsVisible(handle) || PresentationIsIconic(handle) ||
            !PresentationClientRect(handle, out var client) || client.Right < 25 || client.Bottom < 25 ||
            !PresentationWindowRect(handle, out var bounds)) return null;
        var point = screenPoint ?? PresentationClientProbe(client, target == nameof(Window));
        if (screenPoint is null && !PresentationClientToScreen(handle, ref point)) return null;
        nint hitRoot = PresentationRootAtPoint(point);
        bool owned = hitRoot == handle;
        string? pixel = null;
        if (owned)
        {
            nint dc = PresentationGetDC(0);
            try
            {
                uint color = PresentationGetPixel(dc, point.X, point.Y);
                if (color != uint.MaxValue) pixel = $"#{color & 255:X2}{(color >> 8) & 255:X2}{(color >> 16) & 255:X2}";
            }
            finally { PresentationReleaseDC(0, dc); }
        }
        nint foreground = GetForegroundWindow();
        PresentationWindowProcess(foreground, out uint foregroundProcess);
        var frame = new PresentationFrame(caseName, phase, target, "opening-native-poll", _presentationClock.ElapsedMilliseconds,
            0, 0, 0, handle.ToInt64(), true, bounds.Right - bounds.Left, bounds.Bottom - bounds.Top,
            point.X, point.Y, owned, pixel, hitRoot.ToInt64(), foreground.ToInt64(), foregroundProcess,
            PresentationClass(foreground), PresentationClass(hitRoot), sampleStartedTicks, dismissedTicksAtSample, Stopwatch.GetTimestamp());
        _presentationFrames.Enqueue(frame);
        return frame;
    }

    private async Task<Window> TracePresentationOpeningAsync(string route, string state, PresentationEntry entry, Window? existing)
    {
        nint ownerHandle = WinRT.Interop.WindowNative.GetWindowHandle(Window);
        uint guiThread = PresentationWindowProcess(ownerHandle, out uint ownProcess);
        string nativeClass = PresentationClass(ownerHandle), caseName = _presentationCase, phase = _presentationPhase;
        string target = route.StartsWith("model-", StringComparison.Ordinal) ? nameof(ModelManagementWindow) : nameof(Window);
        var previousHandles = PresentationThreadWindows(guiThread, ownProcess, nativeClass).ToHashSet();
        nint existingHandle = existing is null ? 0 : WinRT.Interop.WindowNative.GetWindowHandle(existing);
        // A reused window can extend beyond its owner. Observe only a patch the owner actually covers before activation.
        // 复用窗口可能超出所有者边界；仅观察激活前确由所有者覆盖的两窗交叠区域。
        PresentationPoint? reusePoint = state == "reuse" && existing is not null ? FormerChildPoint(existing, entry.Owner) : null;
        if (state == "reuse" && (reusePoint is null || PresentationRootAtPoint(reusePoint.Value) !=
            WinRT.Interop.WindowNative.GetWindowHandle(entry.Owner)))
            throw new InvalidOperationException("The reuse probe is not covered by its actual owner before opening.");
        PresentationPoint? sourcePoint = null;
        nint sourceHandle = 0;
        string sourceClass = string.Empty;
        if (entry.SourcePopup?.Child is FrameworkElement presenter)
        {
            var position = presenter.TransformToVisual((FrameworkElement)Window.Content)
                .TransformPoint(new Point(presenter.ActualWidth / 2, Math.Max(4, presenter.ActualHeight - 4)));
            double scale = presenter.XamlRoot.RasterizationScale;
            var point = new PresentationPoint { X = (int)Math.Round(position.X * scale), Y = (int)Math.Round(position.Y * scale) };
            if (!PresentationClientToScreen(ownerHandle, ref point)) throw new InvalidOperationException("Cannot locate the actual source popup edge.");
            sourceHandle = PresentationRootAtPoint(point);
            uint sourceThread = PresentationWindowProcess(sourceHandle, out uint sourceProcess);
            Check(sourceProcess == ownProcess && sourceThread == guiThread &&
                (sourceHandle == ownerHandle || PresentationClass(sourceHandle).Contains("PopupWindowSiteBridge", StringComparison.Ordinal)),
                caseName + " source point belongs to this fixture's original popup or main HWND");
            sourcePoint = point;
            sourceClass = PresentationClass(sourceHandle);
        }
        long triggerTicks = 0, dismissedTicks = 0, popupClosedTicks = 0;
        bool hasFlyout = entry.MenuPresenter is not null || entry.PickerFlyout is not null;
        string boundary = entry.PickerFlyout is not null ? "FlyoutBase.Closed" : entry.MenuPresenter is not null ? "MenuFlyoutPresenter.Unloaded" : "none";
        void Dismissed(object? sender, object args) => Interlocked.CompareExchange(ref dismissedTicks, Stopwatch.GetTimestamp(), 0);
        void PresenterUnloaded(object sender, RoutedEventArgs args) => Dismissed(sender, args);
        void PopupClosed(object? sender, object args) => Interlocked.CompareExchange(ref popupClosedTicks, Stopwatch.GetTimestamp(), 0);
        // Gate ordering on presenter unload; Popup.Closed may arrive later.
        // 以 presenter 卸载判断顺序；Popup.Closed 可能迟到，仅记录诊断时间。
        if (entry.MenuPresenter is not null) entry.MenuPresenter.Unloaded += PresenterUnloaded;
        if (entry.MenuPopup is not null) entry.MenuPopup.Closed += PopupClosed;
        if (entry.PickerFlyout is not null) entry.PickerFlyout.Closed += Dismissed;
        using var stopping = new CancellationTokenSource();
        var ready = new TaskCompletionSource(TaskCreationOptions.RunContinuationsAsynchronously);
        // Capture native values before dispatch. The worker never accesses a Window, AppWindow, XamlRoot or flyout.
        // 触发入口前捕获原生值；后台只访问句柄及值快照，不读取任何 WinUI 对象，UI 同步创建期间仍持续采样。
        var sampling = Task.Run(async () =>
        {
            long firstVisible = 0, firstOwned = 0, observedHandle = 0, previousPoll = 0, maximumGap = 0;
            bool visibleBeforeDismissed = false, ownedBeforeDismissed = false;
            int polls = 0;
            if (sourcePoint is { } initialSourcePoint)
                RecordClosingNativeFrame(sourceHandle, "source-popup", initialSourcePoint, caseName, phase, "opening-source-before-trigger",
                    Volatile.Read(ref dismissedTicks));
            ready.TrySetResult();
            while (!stopping.IsCancellationRequested)
            {
                if (Volatile.Read(ref triggerTicks) != 0)
                {
                    long now = Stopwatch.GetTimestamp();
                    if (previousPoll != 0) maximumGap = Math.Max(maximumGap, now - previousPoll);
                    previousPoll = now;
                    polls++;
                    if (sourcePoint is { } popupPoint)
                    {
                        long sourceDismissedTicks = Volatile.Read(ref dismissedTicks);
                        nint hitRoot = PresentationRootAtPoint(popupPoint);
                        if (hitRoot == sourceHandle && PresentationWindowProcess(hitRoot, out uint sourceProcess) == guiThread &&
                            sourceProcess == ownProcess && PresentationClass(hitRoot) == sourceClass)
                            RecordClosingNativeFrame(hitRoot, sourceDismissedTicks == 0 ? "source-popup" : "source-owner",
                                popupPoint, caseName, phase, "opening-source-poll", sourceDismissedTicks);
                        else if (hitRoot == ownerHandle)
                            RecordClosingNativeFrame(ownerHandle, "source-owner", popupPoint, caseName, phase, "opening-source-poll", sourceDismissedTicks);
                    }
                    foreach (nint handle in PresentationThreadWindows(guiThread, ownProcess, nativeClass))
                    {
                        if (existingHandle != 0 ? handle != existingHandle : previousHandles.Contains(handle)) continue;
                        var frame = RecordOpeningNativeFrame(handle, target, caseName, phase, ref dismissedTicks, reusePoint);
                        if (frame is null) continue;
                        long seen = frame.SampleStartedTicks!.Value;
                        long dismissedAtSampleEnd = Volatile.Read(ref dismissedTicks);
                        // Native queries can span content unload. Keep those pixels, but only wholly ordered samples establish ordering.
                        // 原生查询可能跨越内容卸载；保留其像素，但只用完整位于边界一侧的样本判断先后。
                        bool beforeDismissed = hasFlyout && (dismissedAtSampleEnd == 0 || frame.SampleCompletedTicks < dismissedAtSampleEnd);
                        bool afterDismissed = !hasFlyout || dismissedAtSampleEnd > 0 && seen >= dismissedAtSampleEnd;
                        if (firstVisible == 0 && (beforeDismissed || afterDismissed)) { firstVisible = seen; observedHandle = handle.ToInt64(); }
                        if (beforeDismissed) visibleBeforeDismissed = true;
                        if (frame.OwnedPixel && frame.Pixel is not null)
                        {
                            if (firstOwned == 0 && (beforeDismissed || afterDismissed)) firstOwned = seen;
                            if (beforeDismissed) ownedBeforeDismissed = true;
                        }
                    }
                }
                await Task.Delay(2).ConfigureAwait(false);
            }
            long dismissedAt = Volatile.Read(ref dismissedTicks);
            long popupClosedAt = Volatile.Read(ref popupClosedTicks);
            return new OpeningObservation(caseName, state, boundary, observedHandle, Volatile.Read(ref triggerTicks),
                dismissedAt == 0 ? null : dismissedAt, firstVisible,
                firstOwned == 0 ? null : firstOwned, visibleBeforeDismissed, ownedBeforeDismissed, polls, maximumGap,
                popupClosedAt == 0 ? null : popupClosedAt);
        });
        Window? child = null;
        try
        {
            await ready.Task;
            Interlocked.Exchange(ref triggerTicks, Stopwatch.GetTimestamp());
            entry.Invoke();
            if (entry.AfterInvoke is not null) await entry.AfterInvoke();
            await WaitAsync(() => ShellField<Window?>(PresentationChildField(route)) is not null,
                caseName + "-" + state + " dispatches the actual entry before reading the child");
            child = ShellField<Window>(PresentationChildField(route));
            Check(existing is null || ReferenceEquals(existing, child), caseName + "-" + state + " reuses the same production window");
            Check(PresentationTrackedWindows().Contains(child), caseName + "-" + state + " opens a tracked production window");
            RecordPresentationFrame(child, "opener-return");
            nint childHandle = WinRT.Interop.WindowNative.GetWindowHandle(child);
            await WaitAsync(() => !PresentationIsIconic(childHandle) && GetForegroundWindow() == childHandle &&
                child.Content is FrameworkElement { IsLoaded: true }, caseName + "-" + state + " naturally presents loaded content in the foreground");
            if (route.StartsWith("tools-", StringComparison.Ordinal))
            {
                var tools = ShellField<ToolManagementWindow>("_toolManagementWindow");
                await WaitAsync(() => typeof(ToolManagementWindow).GetField("_config", PrivateInstance)!.GetValue(tools) is not null &&
                    !tools.HasPendingChanges, caseName + " loads all four read-only management responses");
                var root = (FrameworkElement)child.Content;
                Check(tools.IsEmbedded && tools.Content is null &&
                    ReferenceEquals(child, ShellField<Window>("_storageSettingsWindow")),
                    caseName + " embeds the real tool controller inside the same visible settings window");
                Check(!NativeUi.ByName<InfoBar>(root, "AgentStatusBar").IsOpen,
                    caseName + " loads the actual tool management page without an error notice");
                int expectedSection = route is "tools-skills" or "tools-drawer" ? 1 :
                    route == "tools-settings" && ShellField<bool>("_settingsToolsSkills") ? 1 : 0;
                Check(NativeUi.ByName<Pivot>(root, "AgentTabs").SelectedIndex == expectedSection,
                    caseName + " selects the section requested by its actual entry");
            }
            await Task.Delay(350);
            RecordPresentationFrame(child, "settled-open");
            return child;
        }
        finally
        {
            stopping.Cancel();
            var observation = await sampling;
            _presentationOpenings.Enqueue(observation);
            if (entry.MenuPresenter is not null) entry.MenuPresenter.Unloaded -= PresenterUnloaded;
            if (entry.MenuPopup is not null) entry.MenuPopup.Closed -= PopupClosed;
            if (entry.PickerFlyout is not null) entry.PickerFlyout.Closed -= Dismissed;
            WritePresentationEvidence();
        }
    }

    private void CheckPresentationOpening(Window child, string state)
    {
        var observation = _presentationOpenings.Last(item => item.Case == _presentationCase && item.State == state);
        nint handle = WinRT.Interop.WindowNative.GetWindowHandle(child);
        var frames = _presentationFrames.Where(frame => frame.Case == _presentationCase &&
            frame.Phase == "opening-" + state && frame.Handle == handle.ToInt64()).ToArray();
        var nativeFrames = frames.Where(frame => frame.Source == "opening-native-poll").ToArray();
        string label = _presentationCase + "-" + state;
        Check(observation.Handle == handle.ToInt64() && observation.FirstVisibleTicks >= observation.TriggerTicks,
            label + " observes the exact production HWND from a native sampler armed before the trigger");
        Check(nativeFrames.Count(frame => frame.OwnedPixel && frame.Pixel is not null) >= 3,
            label + " records multiple verified composed pixels before settling");
        var first = nativeFrames.First();
        var settled = frames.Last(frame => frame.Source == "settled-open");
        Check(first.Width == settled.Width && first.Height == settled.Height,
            label + " first observed visible bounds match settled bounds without a test resize");
        if (observation.DismissBoundary != "none")
        {
            // An existing window stays WS_VISIBLE behind its owner; its newly exposed pixel is the relevant reuse boundary.
            // 已有窗口在所有者后方仍保持 WS_VISIBLE；复用时以重新露出的自有像素作为展示边界。
            bool ordered = state == "fresh" ? !observation.VisibleBeforeDismissed &&
                observation.DismissedTicks <= observation.FirstVisibleTicks : !observation.OwnedPixelBeforeDismissed &&
                observation.DismissedTicks <= observation.FirstOwnedPixelTicks;
            Check(observation.DismissedTicks is not null && ordered,
                label + " presents after the original " + observation.DismissBoundary + " dismissal boundary");
        }
        if (_presentationCase.StartsWith("dark-", StringComparison.Ordinal))
        {
            Check(settled.Pixel is not null && !PresentationWhite(settled.Pixel), label + " settles to an actually dark child surface");
            Check(!nativeFrames.Any(frame => frame.Pixel is not null && PresentationWhite(frame.Pixel)),
                label + " has no sampled white child frame from its first observed presentation");
        }
        else
        {
            Check(settled.Pixel == AppearanceService.Current.Main, label + " settles to the actual light Main background after the native show transition");
        }
        if (observation.DismissBoundary != "none")
        {
            var sourceFrames = _presentationFrames.Where(frame => frame.Case == _presentationCase &&
                frame.Phase == "opening-" + state && frame.Source.StartsWith("opening-source-", StringComparison.Ordinal) &&
                frame.OwnedPixel && frame.Pixel is not null).ToArray();
            Check(sourceFrames.Length > 0, label + " samples the original popup edge and continues where the main region remains exposed");
            if (_presentationCase.StartsWith("dark-", StringComparison.Ordinal))
                Check(!sourceFrames.Any(frame => PresentationWhite(frame.Pixel!)), label + " has no sampled white source popup or exposed main region");
        }
    }

    private async Task CheckWindowPresentationAsync()
    {
        var original = AppearanceService.Current;
        string mode = AppearanceService.Mode;
        double fontSize = AppearanceService.ChatFontSize;
        string? accent = AppearanceService.CustomAccent;
        string draft = Prompt.Text;
        Guid? chatId = ActiveId;
        int writes = _gateway.Writes;
        var originalSize = Window.AppWindow.Size;
        var originalPosition = Window.AppWindow.Position;
        var layout = ShellField<LayoutState>("_layout");
        bool originalCollapsed = layout.SidebarCollapsed;
        Window? child = null;
        Window? entryOwner = null;
        PresentationWinEvent callback = (_, eventId, handle, objectId, childId, _, eventTime) =>
        {
            // OUTOFCONTEXT callbacks return safely to Windows even if managed diagnostics fail.
            // OUTOFCONTEXT 回调即使诊断失败也安全返回 Windows，不让托管异常跨越原生边界。
            try
            {
                if (objectId != 0 || childId != 0 || PresentationGetAncestor(handle, 2) != handle) return;
                var tracked = PresentationTrackedWindows().FirstOrDefault(window => WinRT.Interop.WindowNative.GetWindowHandle(window) == handle);
                if (tracked is not null) RecordPresentationFrame(tracked, "win-event", eventId, eventTime);
            }
            catch (Exception error) { _presentationHookError ??= error; }
        };
        nint hook = PresentationSetWinEventHook(0x8002, 0x800B, 0, callback, (uint)Environment.ProcessId, 0, 0);
        if (hook == 0) throw new InvalidOperationException("Cannot observe this fixture's native window show/location events.");
        try
        {
            await ActivateOwnedShortcutWindowAsync();
            foreach (string selectedMode in new[] { "dark", "light" })
            {
                AppearanceService.ApplyPreferences("mist-blue", selectedMode, fontSize, null);
                await SettleAsync();
                foreach (string route in new[] { "model-more", "tools-more", "settings-more", "model-sidebar", "model-picker",
                    "model-status", "tools-skills", "tools-settings", "settings-sidebar", "retrieval-knowledge",
                    "model-drawer", "tools-drawer", "settings-drawer", "settings-rail" })
                {
                    bool closed = false;
                    string[] states = route == "retrieval-knowledge" ? ["fresh"] :
                        route.EndsWith("-drawer", StringComparison.Ordinal) || route == "settings-rail" ? ["fresh", "reuse"] : ["fresh", "reuse", "restore"];
                    foreach (string state in states)
                    {
                        _presentationCase = string.Empty;
                        _presentationPhase = string.Empty;
                        if (state == "restore")
                        {
                            Check(child!.AppWindow.Presenter is OverlappedPresenter, route + " has a minimizable production presenter");
                            ((OverlappedPresenter)child.AppWindow.Presenter).Minimize();
                            nint minimized = WinRT.Interop.WindowNative.GetWindowHandle(child);
                            await WaitAsync(() => PresentationIsIconic(minimized), route + " actually minimizes before exercising its restore entry");
                        }
                        var entry = await PreparePresentationEntryAsync(route);
                        entryOwner = entry.Owner;
                        _presentationCase = selectedMode + "-" + route;
                        _presentationPhase = "opening-" + state;
                        _presentationClock.Restart();
                        RecordPresentationFrame(entry.Owner, "before-open", target: "owner");
                        child = await TracePresentationOpeningAsync(route, state, entry, child);
                        if (state == "fresh")
                        {
                            child.Closed += (_, _) => closed = true;
                            AttachEraseContractProbe(child);
                        }
                        CheckPresentationOpening(child, state);
                    }
                    if (child is null) throw new InvalidOperationException("The actual entry produced no window to close.");
                    await NativeWindowCapture.CaptureAsync(child, Path.Combine(_directory, "window-presentation-" + _presentationCase + "-open.png"));
                    var exposurePoint = FormerChildPoint(child, entryOwner!);
                    Check(exposurePoint is not null, _presentationCase + " has a verified former-child patch inside the actual owner client area");
                    _presentationPhase = "closing";
                    await TraceNativeCloseAsync(child, exposurePoint!.Value, entryOwner);
                    await WaitAsync(() => closed, _presentationCase + " closes through actual native WM_CLOSE without a pending confirmation");
                    _presentationPhase = "owner-restored";
                    await TracePresentationForAsync(child, TimeSpan.FromMilliseconds(150), exposurePoint, () => closed, entryOwner);
                    RecordPresentationFrame(entryOwner!, "settled-owner", screenPoint: exposurePoint, target: "owner");
                    await NativeWindowCapture.CaptureAsync(entryOwner!, Path.Combine(_directory, "window-presentation-" + _presentationCase + "-closed.png"));
                    child = null;
                    _presentationCase = string.Empty;
                    _presentationPhase = string.Empty;
                    if (!ReferenceEquals(entryOwner, Window))
                    {
                        NativeUi.RequestClose(entryOwner!);
                        await WaitAsync(() => ShellField<Window?>("_storageSettingsWindow") is null,
                            "the prerequisite settings window closes without a formal write");
                    }
                    entryOwner = null;
                    await SettleAsync();
                }
            }
            Check(_presentationHookError is null, "native presentation callbacks complete without crossing an exception into Windows");
            WritePresentationEvidence();
            foreach (string caseName in _presentationFrames.Select(frame => frame.Case).Where(name => name.Length > 0).Distinct())
            {
                var frames = _presentationFrames.Where(frame => frame.Case == caseName).ToArray();
                var childFrames = frames.Where(frame => frame.Target != "owner" && frame.Phase == "opening-fresh").ToArray();
                Check(childFrames.Any(frame => frame.NativeEvent == 0x8002), caseName + " observes its native top-level show event");
                var ownerFrames = frames.Where(frame => frame.Target == "owner" && (frame.Phase is "closing" or "owner-restored") && frame.OwnedPixel && frame.Pixel is not null).ToArray();
                Check(ownerFrames.Length >= 3, caseName + " records actual owner pixels in the child's former area after X closing");
                if (caseName.StartsWith("dark-", StringComparison.Ordinal))
                {
                    Check(!PresentationWhite(ownerFrames[^1].Pixel!), caseName + " exposes an actually dark owner surface after close");
                    Check(!ownerFrames.Any(frame => PresentationWhite(frame.Pixel!)), caseName + " has no sampled white owner frame during native closing");
                }
                var erase = _presentationEraseContracts.SingleOrDefault(observation => observation.Case == caseName);
                Check(erase is not null && erase.Error is null && erase.ActualColorRef == erase.ExpectedColorRef,
                    caseName + " native teardown erase contract matches the actual current Main color in its offscreen bitmap");
            }
            Check(Prompt.Text == draft && ActiveId == chatId && _gateway.Writes == writes && _gateway.Failure is null && _unhandled is null,
                "focused window presentation preserves the actual owner draft and conversation without formal writes");
        }
        finally
        {
            PresentationUnhookWinEvent(hook);
            GC.KeepAlive(callback);
            foreach (var registration in _presentationEraseHooks)
                if (PresentationIsWindow(registration.Handle))
                    PresentationRemoveSubclass(registration.Handle, registration.Callback, PresentationEraseSubclassId);
            _presentationEraseHooks.Clear();
            if (child is not null && PresentationTrackedWindows().Contains(child)) child.Close();
            if (entryOwner is not null && !ReferenceEquals(entryOwner, Window) && PresentationTrackedWindows().Contains(entryOwner)) entryOwner.Close();
            layout.SidebarCollapsed = originalCollapsed;
            SetShellField("_temporarySidebarOpen", false);
            Window.AppWindow.MoveAndResize(new Windows.Graphics.RectInt32(originalPosition.X, originalPosition.Y, originalSize.Width, originalSize.Height));
            Call("ApplyLayout");
            Call("SaveLayout");
            AppearanceService.ApplyPreferences(original.Id, mode, fontSize, accent);
            WritePresentationEvidence();
        }
    }

    private void WritePresentationEvidence() => File.WriteAllText(Path.Combine(_directory, "window-presentation-frames.json"),
        JsonSerializer.Serialize(new { Samples = _presentationFrames, CallbackError = _presentationHookError?.ToString(),
            EraseContracts = _presentationEraseContracts,
            Openings = _presentationOpenings, StopwatchFrequency = Stopwatch.Frequency,
            Limitation = "Native polling starts before the actual entry trigger, but natural display samples can miss shorter frames. SampleStartedTicks and DismissedTicksAtSample are captured before native queries/pixel I/O; SampleCompletedTicks exposes samples spanning a concurrent close event. Poll intervals and maximum gaps are evidence, not guarantees of refresh-frame coverage. Source popup points are sampled before dispatch and through dismissal only while hit testing identifies the original own popup or main HWND; a new child occluding that point is skipped. More menu ordering observes its original MenuFlyoutPresenter.Unloaded content-unload boundary, not native animation completion. More's original Popup.Closed callback is recorded separately as PopupClosedTicks; it can arrive asynchronously and is not an ordering gate or proof of render completion. Model picker dismissal observes its original FlyoutBase.Closed. Existing windows remain WS_VISIBLE behind the owner, so reuse ordering checks their first verified exposed pixel. Settings samples its verified left content gutter, away from the right ScrollBar arrow. Light checks the settled current Main background because natural show-animation blending can change its first pixel. EraseContracts are separate offscreen 8x8 WM_DESTROY contract probes, not naturally displayed white frames; ordinary erase messages are not replaced." },
            new JsonSerializerOptions { WriteIndented = true }));

    private static nint PresentationRootAtPoint(PresentationPoint point) => PresentationGetAncestor(PresentationWindowFromPoint(point), 2);
    [StructLayout(LayoutKind.Sequential)] private struct PresentationPoint { public int X, Y; }
    [StructLayout(LayoutKind.Sequential)] private struct PresentationRect { public int Left, Top, Right, Bottom; }
    private delegate void PresentationWinEvent(nint hook, uint eventId, nint window, int objectId, int childId, uint thread, uint eventTime);
    [UnmanagedFunctionPointer(CallingConvention.Winapi)]
    private delegate bool PresentationEnumWindow(nint window, nint parameter);
    [UnmanagedFunctionPointer(CallingConvention.Winapi)]
    private delegate nint PresentationEraseSubclass(nint window, uint message, nint wParam, nint lParam, nuint subclassId, nuint referenceData);
    [DllImport("comctl32.dll", EntryPoint = "SetWindowSubclass", ExactSpelling = true)] private static extern bool PresentationSetSubclass(nint window, PresentationEraseSubclass callback, nuint subclassId, nuint referenceData);
    [DllImport("comctl32.dll", EntryPoint = "RemoveWindowSubclass", ExactSpelling = true)] private static extern bool PresentationRemoveSubclass(nint window, PresentationEraseSubclass callback, nuint subclassId);
    [DllImport("comctl32.dll", EntryPoint = "DefSubclassProc", ExactSpelling = true)] private static extern nint PresentationDefSubclass(nint window, uint message, nint wParam, nint lParam);
    [DllImport("gdi32.dll", EntryPoint = "CreateCompatibleDC")] private static extern nint PresentationCreateCompatibleDC(nint source);
    [DllImport("gdi32.dll", EntryPoint = "CreateCompatibleBitmap")] private static extern nint PresentationCreateCompatibleBitmap(nint dc, int width, int height);
    [DllImport("gdi32.dll", EntryPoint = "SelectObject")] private static extern nint PresentationSelectObject(nint dc, nint value);
    [DllImport("gdi32.dll", EntryPoint = "CreateSolidBrush")] private static extern nint PresentationCreateSolidBrush(uint color);
    [DllImport("user32.dll", EntryPoint = "FillRect")] private static extern int PresentationFillRect(nint dc, ref PresentationRect rectangle, nint brush);
    [DllImport("gdi32.dll", EntryPoint = "DeleteObject")] private static extern bool PresentationDeleteObject(nint value);
    [DllImport("gdi32.dll", EntryPoint = "DeleteDC")] private static extern bool PresentationDeleteDC(nint dc);
    [DllImport("user32.dll", EntryPoint = "SetWinEventHook")] private static extern nint PresentationSetWinEventHook(uint minimum, uint maximum, nint module, PresentationWinEvent callback, uint process, uint thread, uint flags);
    [DllImport("user32.dll", EntryPoint = "UnhookWinEvent")] private static extern bool PresentationUnhookWinEvent(nint hook);
    [DllImport("user32.dll", EntryPoint = "IsWindow")] private static extern bool PresentationIsWindow(nint window);
    [DllImport("user32.dll", EntryPoint = "IsWindowVisible")] private static extern bool PresentationIsVisible(nint window);
    [DllImport("user32.dll", EntryPoint = "IsIconic")] private static extern bool PresentationIsIconic(nint window);
    [DllImport("user32.dll", EntryPoint = "EnumThreadWindows")] private static extern bool PresentationEnumThreadWindows(uint thread, PresentationEnumWindow callback, nint parameter);
    [DllImport("user32.dll", EntryPoint = "GetClientRect")] private static extern bool PresentationClientRect(nint window, out PresentationRect rect);
    [DllImport("user32.dll", EntryPoint = "GetWindowRect")] private static extern bool PresentationWindowRect(nint window, out PresentationRect rect);
    [DllImport("user32.dll", EntryPoint = "GetWindowThreadProcessId")] private static extern uint PresentationWindowProcess(nint window, out uint process);
    [DllImport("user32.dll", EntryPoint = "GetClassNameW", CharSet = CharSet.Unicode)] private static extern int PresentationClassName(nint window, StringBuilder name, int capacity);
    [DllImport("user32.dll", EntryPoint = "ClientToScreen")] private static extern bool PresentationClientToScreen(nint window, ref PresentationPoint point);
    [DllImport("user32.dll", EntryPoint = "WindowFromPoint")] private static extern nint PresentationWindowFromPoint(PresentationPoint point);
    [DllImport("user32.dll", EntryPoint = "GetAncestor")] private static extern nint PresentationGetAncestor(nint window, uint flags);
    [DllImport("user32.dll", EntryPoint = "GetDC")] private static extern nint PresentationGetDC(nint window);
    [DllImport("user32.dll", EntryPoint = "ReleaseDC")] private static extern int PresentationReleaseDC(nint window, nint dc);
    [DllImport("gdi32.dll", EntryPoint = "GetPixel")] private static extern uint PresentationGetPixel(nint dc, int x, int y);
}

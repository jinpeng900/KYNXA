using System.Collections.Concurrent;
using System.Diagnostics;
using System.Reflection;
using System.Runtime.InteropServices;
using System.Text.Json;
using System.Text;
using KYNXA_Desktop.Services;
using MemoryUiSmoke;
using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Automation;
using Microsoft.UI.Xaml.Controls;

namespace KYNXA_Desktop;

public partial class App
{
    private sealed record PresentationFrame(string Case, string Phase, string Target, string Source,
        long Milliseconds, uint NativeEvent, uint EventTime, uint DeliveryDelayMs, long Handle,
        bool Visible, int Width, int Height, int ScreenX, int ScreenY, bool OwnedPixel, string? Pixel,
        long HitRoot, long Foreground, uint ForegroundProcess, string ForegroundClass, string HitClass);

    private readonly ConcurrentQueue<PresentationFrame> _presentationFrames = new();
    private string _presentationCase = string.Empty;
    private string _presentationPhase = string.Empty;
    private readonly Stopwatch _presentationClock = new();
    private Exception? _presentationHookError;
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

    private PresentationFrame? RecordPresentationFrame(Window window, string source,
        uint nativeEvent = 0, uint eventTime = 0, PresentationPoint? screenPoint = null)
    {
        if (_presentationCase.Length == 0) return null;
        nint handle = WinRT.Interop.WindowNative.GetWindowHandle(window);
        // Never read a destroyed or hidden child's display surface; its former area belongs to the owner probe.
        // 不读取已销毁或隐藏的附属窗口显示面；其原位置交由所有者窗口探测。
        if (!PresentationIsWindow(handle) || !PresentationIsVisible(handle)) return null;
        if (!PresentationClientRect(handle, out var client) || client.Right < 25 || client.Bottom < 25) return null;
        var point = screenPoint ?? new PresentationPoint { X = client.Right - 12, Y = client.Bottom - 12 };
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
            ReferenceEquals(window, Window) ? "owner" : window.GetType().Name, source,
            _presentationClock.ElapsedMilliseconds, nativeEvent, eventTime,
            eventTime == 0 ? 0 : unchecked((uint)Environment.TickCount - eventTime), handle.ToInt64(),
            true, size.Width, size.Height, point.X, point.Y, owned, pixel,
            hitRoot.ToInt64(), foreground.ToInt64(), foregroundProcess, PresentationClass(foreground), PresentationClass(hitRoot));
        _presentationFrames.Enqueue(frame);
        return frame;
    }

    private void RecordClosingNativeFrame(nint handle, string target, PresentationPoint point, string caseName)
    {
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
        _presentationFrames.Enqueue(new(caseName, "closing", target, "close-native-poll", _presentationClock.ElapsedMilliseconds,
            0, 0, 0, handle.ToInt64(), true, bounds.Right - bounds.Left, bounds.Bottom - bounds.Top,
            point.X, point.Y, owned, pixel, hitRoot.ToInt64(), foreground.ToInt64(), process,
            PresentationClass(foreground), PresentationClass(hitRoot)));
    }

    private async Task TraceNativeCloseAsync(Window child, PresentationPoint ownerPoint)
    {
        nint childHandle = WinRT.Interop.WindowNative.GetWindowHandle(child);
        nint ownerHandle = WinRT.Interop.WindowNative.GetWindowHandle(Window);
        PresentationClientRect(childHandle, out var client);
        var childPoint = new PresentationPoint { X = client.Right - 12, Y = client.Bottom - 12 };
        PresentationClientToScreen(childHandle, ref childPoint);
        string target = child.GetType().Name, caseName = _presentationCase;
        var started = new TaskCompletionSource(TaskCreationOptions.RunContinuationsAsynchronously);
        // This sampler owns only captured native handles and coordinates, so it keeps observing during synchronous SDK teardown.
        // 采样器仅持有已捕获的原生句柄和坐标，SDK 同步关闭阻塞 UI 线程时仍可观察真实合成帧。
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
        Func<bool>? childClosed = null)
    {
        var elapsed = Stopwatch.StartNew();
        while (elapsed.Elapsed < duration)
        {
            if (childClosed?.Invoke() != true) RecordPresentationFrame(child, "display-poll");
            RecordPresentationFrame(Window, "display-poll", screenPoint: ownerPoint);
            await Task.Delay(8);
        }
    }

    private static bool PresentationWhite(string color) =>
        Convert.ToInt32(color.Substring(1, 2), 16) >= 240 &&
        Convert.ToInt32(color.Substring(3, 2), 16) >= 240 &&
        Convert.ToInt32(color.Substring(5, 2), 16) >= 240;

    private async Task CheckWindowPresentationAsync()
    {
        var original = AppearanceService.Current;
        string mode = AppearanceService.Mode;
        double fontSize = AppearanceService.ChatFontSize;
        string? accent = AppearanceService.CustomAccent;
        string draft = Prompt.Text;
        Guid? chatId = ActiveId;
        int writes = _gateway.Writes;
        Window? child = null;
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
                foreach (string kind in new[] { "model", "settings", "retrieval" })
                {
                    await ActivateOwnedShortcutWindowAsync();
                    _presentationCase = selectedMode + "-" + kind;
                    _presentationPhase = "opening";
                    _presentationClock.Restart();
                    RecordPresentationFrame(Window, "before-open");
                    if (kind == "model")
                    {
                        Call("OpenModelManagement", false);
                        child = ShellField<Window>("_modelManagementWindow");
                    }
                    else if (kind == "settings")
                    {
                        Call("StorageSettings_Click", _shell, new RoutedEventArgs());
                        child = ShellField<Window>("_storageSettingsWindow");
                    }
                    else
                    {
                        var knowledge = NativeUi.Descendants<Button>(_shell).Single(button =>
                            AutomationProperties.GetName(button) == UiText.Get("知识库") && NativeUi.IsVisible(button));
                        NativeUi.Invoke(knowledge);
                        await WaitAsync(() => CurrentRetrievalWindow is not null,
                            _presentationCase + " dispatches the actual knowledge button before reading its owner reference");
                        child = CurrentRetrievalWindow;
                    }
                    Check(child is not null && PresentationTrackedWindows().Contains(child),
                        _presentationCase + " opens the tracked production window through its actual Shell entry");
                    bool closed = false;
                    child!.Closed += (_, _) => closed = true;
                    AttachEraseContractProbe(child);
                    child.AppWindow.Show(activateWindow: true);
                    child.Activate();
                    SetForegroundWindow(WinRT.Interop.WindowNative.GetWindowHandle(child));
                    child.AppWindow.MoveInZOrderAtTop();
                    RecordPresentationFrame(child, "opener-return");
                    await WaitAsync(() => GetForegroundWindow() == WinRT.Interop.WindowNative.GetWindowHandle(child),
                        _presentationCase + " acquires its own foreground before native pixel sampling");
                    await TracePresentationForAsync(child, TimeSpan.FromMilliseconds(350));
                    await WaitAsync(() => child.Content is FrameworkElement { IsLoaded: true },
                        _presentationCase + " realizes its actual content without a test resize");
                    RecordPresentationFrame(child, "settled-open");
                    await NativeWindowCapture.CaptureAsync(child, Path.Combine(_directory, "window-presentation-" + _presentationCase + "-open.png"));
                    var exposurePoint = FormerChildPoint(child, Window);
                    Check(exposurePoint is not null, _presentationCase + " has a verified former-child patch inside the actual owner client area");
                    _presentationPhase = "closing";
                    await TraceNativeCloseAsync(child, exposurePoint!.Value);
                    await WaitAsync(() => closed, _presentationCase + " closes through actual native WM_CLOSE without a pending confirmation");
                    _presentationPhase = "owner-restored";
                    await TracePresentationForAsync(child, TimeSpan.FromMilliseconds(150), exposurePoint, () => closed);
                    RecordPresentationFrame(Window, "settled-owner", screenPoint: exposurePoint);
                    await NativeWindowCapture.CaptureAsync(Window, Path.Combine(_directory, "window-presentation-" + _presentationCase + "-closed.png"));
                    child = null;
                    _presentationCase = string.Empty;
                    _presentationPhase = string.Empty;
                    await SettleAsync();
                }
            }
            Check(_presentationHookError is null, "native presentation callbacks complete without crossing an exception into Windows");
            WritePresentationEvidence();
            foreach (string caseName in _presentationFrames.Select(frame => frame.Case).Where(name => name.Length > 0).Distinct())
            {
                var frames = _presentationFrames.Where(frame => frame.Case == caseName).ToArray();
                var childFrames = frames.Where(frame => frame.Target != "owner" && frame.Phase == "opening").ToArray();
                Check(childFrames.Any(frame => frame.NativeEvent == 0x8002), caseName + " observes its native top-level show event");
                var first = childFrames.FirstOrDefault(frame => frame.NativeEvent == 0x8002) ?? childFrames.First();
                var settled = childFrames.Last(frame => frame.Source == "settled-open");
                Check(first.Width == settled.Width && first.Height == settled.Height,
                    caseName + " first observed visible bounds match settled bounds without opening resize");
                Check(childFrames.Count(frame => frame.OwnedPixel && frame.Pixel is not null) >= 3,
                    caseName + " records multiple actual composed child pixels from first opening");
                var ownerFrames = frames.Where(frame => frame.Target == "owner" && (frame.Phase is "closing" or "owner-restored") && frame.OwnedPixel && frame.Pixel is not null).ToArray();
                Check(ownerFrames.Length >= 3, caseName + " records actual owner pixels in the child's former area after X closing");
                if (caseName.StartsWith("dark-", StringComparison.Ordinal))
                {
                    Check(settled.Pixel is not null && !PresentationWhite(settled.Pixel), caseName + " settles to an actually dark child surface");
                    Check(!childFrames.Any(frame => frame.Pixel is not null && PresentationWhite(frame.Pixel)), caseName + " has no sampled white child frame during opening");
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
            AppearanceService.ApplyPreferences(original.Id, mode, fontSize, accent);
            WritePresentationEvidence();
        }
    }

    private void WritePresentationEvidence() => File.WriteAllText(Path.Combine(_directory, "window-presentation-frames.json"),
        JsonSerializer.Serialize(new { Samples = _presentationFrames, CallbackError = _presentationHookError?.ToString(),
            EraseContracts = _presentationEraseContracts,
            Limitation = "Natural display samples can miss shorter frames. EraseContracts are separate offscreen 8x8 WM_DESTROY contract probes, not naturally displayed white frames; ordinary erase messages are not replaced." },
            new JsonSerializerOptions { WriteIndented = true }));

    private static nint PresentationRootAtPoint(PresentationPoint point) => PresentationGetAncestor(PresentationWindowFromPoint(point), 2);
    [StructLayout(LayoutKind.Sequential)] private struct PresentationPoint { public int X, Y; }
    [StructLayout(LayoutKind.Sequential)] private struct PresentationRect { public int Left, Top, Right, Bottom; }
    private delegate void PresentationWinEvent(nint hook, uint eventId, nint window, int objectId, int childId, uint thread, uint eventTime);
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

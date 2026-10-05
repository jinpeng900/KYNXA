using System.Diagnostics;
using static KYNXA.ToolHost.DesktopNativeMethods;

namespace KYNXA.ToolHost;

/// <summary>Bounded placement of explicitly owned windows without minimizing or bypassing focus policy.</summary>
/// <remarks>只在有限范围内调整明确归属的窗口，不最小化窗口或绕过焦点策略。</remarks>
internal sealed class DesktopForegroundPlacement
{
    private const int LaunchObservationMs = 1500;
    private readonly nint _initialForegroundWindow = GetForegroundWindow();
    private readonly Dictionary<nint, int> _ownedWindows = [];
    private nint _protectedForegroundWindow;
    private uint _protectedProcessId;

    internal DesktopForegroundPlacement() => RememberForeground(_initialForegroundWindow);

    internal bool FocusRestoreAttempted { get; private set; }
    internal bool FocusRestoreSucceeded { get; private set; }
    internal bool WindowObserved => _ownedWindows.Count != 0;
    internal bool ForegroundPreserved => _initialForegroundWindow == GetForegroundWindow();
    internal bool WindowPlacedBehind => _ownedWindows.Count != 0 && _ownedWindows.All(window =>
        HasIdentity(window.Key, window.Value) && IsBehind(window.Key, _protectedForegroundWindow));

    internal void ObserveLaunch(int processId, CancellationToken cancellation)
    {
        var elapsed = Stopwatch.StartNew();
        while (elapsed.ElapsedMilliseconds < LaunchObservationMs)
        {
            EnumWindows((window, _) =>
            {
                if (HasIdentity(window, processId) && IsWindowVisible(window)) PlaceBehind(window, processId);
                return true;
            }, 0);
            // Startup observation never cancels/replays an already launched application's side effects.
            // 启动观察不会取消或重放已启动应用产生的副作用。
            if (cancellation.WaitHandle.WaitOne(25)) break;
        }
    }

    internal void PlaceBehind(nint window, int processId)
    {
        if (!HasIdentity(window, processId) || !IsWindowVisible(window)) return;
        nint current = GetForegroundWindow();
        if (current != 0 && current != window) RememberForeground(current);
        // Operating on a window already selected by the user must not demote their current window.
        // 操作用户当前选中的窗口时，不应将该窗口置于后台。
        if (_protectedForegroundWindow == window) return;
        _ownedWindows[window] = processId;
        // Place just behind the selected ordinary window, instead of behind every application.
        // A topmost foreground uses HWND_BOTTOM to avoid making the target itself topmost.
        // ASYNCWINDOWPOS avoids waiting on a slow app's thread; capture never needs activation.
        // 将目标放在当前普通窗口之后；若前台窗口置顶，则使用 HWND_BOTTOM，避免目标也被置顶。
        // ASYNCWINDOWPOS 避免等待慢应用线程，截屏无需激活窗口。
        nint precedingWindow = ProtectedForegroundExists() && (GetWindowLong(_protectedForegroundWindow, -20) & 0x8) == 0
            ? _protectedForegroundWindow : 1;
        SetWindowPos(window, precedingWindow, 0, 0, 0, 0, 0x0001 | 0x0002 | 0x0010 | 0x0200 | 0x4000);
        if (current != window || GetForegroundWindow() != window || !ProtectedForegroundExists()) return;
        // Only undo this known target's startup activation. A user switching to an unrelated
        // window updates the protected identity instead of having focus pulled back to KYNXA.
        // 只纠正已知目标启动时的抢焦点；用户切换到其他窗口时更新受保护身份，不把焦点强行拉回 KYNXA。
        FocusRestoreAttempted = true;
        FocusRestoreSucceeded |= SetForegroundWindow(_protectedForegroundWindow);
    }

    private void RememberForeground(nint window)
    {
        if (window == 0 || !IsWindow(window)) return;
        GetWindowThreadProcessId(window, out uint processId);
        _protectedForegroundWindow = window;
        _protectedProcessId = processId;
    }

    private bool ProtectedForegroundExists() => _protectedProcessId != 0 &&
        HasIdentity(_protectedForegroundWindow, checked((int)_protectedProcessId)) &&
        IsWindowVisible(_protectedForegroundWindow) && !IsIconic(_protectedForegroundWindow);

    private static bool HasIdentity(nint window, int processId)
    {
        if (!IsWindow(window)) return false;
        GetWindowThreadProcessId(window, out uint ownerProcessId);
        return ownerProcessId == processId;
    }

    private static bool IsBehind(nint window, nint foreground)
    {
        if (foreground == 0 || foreground == window || !IsWindow(foreground)) return false;
        for (int index = 0; index < 10000 && window != 0; index++)
        {
            window = GetWindow(window, 3); // GW_HWNDPREV: higher windows in the same Z-order. 中文：GW_HWNDPREV：同一 Z 序中位于目标上方的窗口。
            if (window == foreground) return true;
        }
        return false;
    }
}

using System.Diagnostics;
using static KYNXA.ToolHost.DesktopNativeMethods;

namespace KYNXA.ToolHost;

/// <summary>Bounded placement of explicitly owned windows without minimizing or bypassing focus policy.</summary>
internal sealed class DesktopForegroundPlacement
{
    private const int LaunchObservationMs = 1500;
    private readonly nint _initialForeground = GetForegroundWindow();
    private readonly Dictionary<nint, int> _windows = [];
    private nint _protectedForeground;
    private uint _protectedProcessId;

    internal DesktopForegroundPlacement() => RememberForeground(_initialForeground);

    internal bool FocusRestoreAttempted { get; private set; }
    internal bool FocusRestoreSucceeded { get; private set; }
    internal bool WindowObserved => _windows.Count != 0;
    internal bool ForegroundPreserved => _initialForeground == GetForegroundWindow();
    internal bool WindowPlacedBehind => _windows.Count != 0 && _windows.All(window =>
        HasIdentity(window.Key, window.Value) && IsBehind(window.Key, _protectedForeground));

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
            if (cancellation.WaitHandle.WaitOne(25)) break;
        }
    }

    internal void PlaceBehind(nint window, int processId)
    {
        if (!HasIdentity(window, processId) || !IsWindowVisible(window)) return;
        nint current = GetForegroundWindow();
        if (current != 0 && current != window) RememberForeground(current);
        // Operating on a window already selected by the user must not demote their current window.
        if (_protectedForeground == window) return;
        _windows[window] = processId;
        // Place just behind the selected ordinary window, instead of behind every application.
        // A topmost foreground uses HWND_BOTTOM to avoid making the target itself topmost.
        // ASYNCWINDOWPOS avoids waiting on a slow app's thread; capture never needs activation.
        nint preceding = ProtectedForegroundExists() && (GetWindowLong(_protectedForeground, -20) & 0x8) == 0
            ? _protectedForeground : 1;
        SetWindowPos(window, preceding, 0, 0, 0, 0, 0x0001 | 0x0002 | 0x0010 | 0x0200 | 0x4000);
        if (current != window || GetForegroundWindow() != window || !ProtectedForegroundExists()) return;
        // Only undo this known target's startup activation. A user switching to an unrelated
        // window updates the protected identity instead of having focus pulled back to KYNXA.
        FocusRestoreAttempted = true;
        FocusRestoreSucceeded |= SetForegroundWindow(_protectedForeground);
    }

    private void RememberForeground(nint window)
    {
        if (window == 0 || !IsWindow(window)) return;
        GetWindowThreadProcessId(window, out uint processId);
        _protectedForeground = window;
        _protectedProcessId = processId;
    }

    private bool ProtectedForegroundExists() => _protectedProcessId != 0 &&
        HasIdentity(_protectedForeground, checked((int)_protectedProcessId)) &&
        IsWindowVisible(_protectedForeground) && !IsIconic(_protectedForeground);

    private static bool HasIdentity(nint window, int processId)
    {
        if (!IsWindow(window)) return false;
        GetWindowThreadProcessId(window, out uint owner);
        return owner == processId;
    }

    private static bool IsBehind(nint window, nint foreground)
    {
        if (foreground == 0 || foreground == window || !IsWindow(foreground)) return false;
        for (int index = 0; index < 10000 && window != 0; index++)
        {
            window = GetWindow(window, 3); // GW_HWNDPREV: higher windows in the same Z-order.
            if (window == foreground) return true;
        }
        return false;
    }
}

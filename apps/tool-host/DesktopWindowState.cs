using static KYNXA.ToolHost.DesktopNativeMethods;

namespace KYNXA.ToolHost;

internal static class DesktopWindowState
{
    internal static Dictionary<string, object?> Apply(DesktopTarget target, DesktopRequest request)
    {
        target.Check(visible: false); target.EnsureIntegrity(); target.EnsureResponding();
        nint foregroundBefore = GetForegroundWindow();
        var placement = new DesktopForegroundPlacement();
        switch (request.Mode)
        {
            case "resize":
                if (request.Width is not >= 64 or > 8192 || request.Height is not >= 64 or > 8192)
                    throw new DesktopException("DESKTOP_INVALID_REQUEST", "Resize requires bounded client width and height.");
                if (IsIconic(target.Window) || IsZoomed(target.Window))
                    throw new DesktopException("DESKTOP_WINDOW_UNAVAILABLE", "Restore the window before resizing its client area.");
                Rect client = target.ClientRect();
                if (!GetWindowRect(target.Window, out Rect outer) || !SetWindowPos(target.Window, 0, 0, 0,
                    request.Width.Value + outer.Right - outer.Left - client.Right,
                    request.Height.Value + outer.Bottom - outer.Top - client.Bottom, 0x0002 | 0x0004 | 0x0010))
                    throw new DesktopException("DESKTOP_WINDOW_FAILED", "Windows rejected the requested window size.");
                break;
            case "maximize":
                // Targeted system command maximizes without an explicit activation request.
                // 向目标窗口发送系统命令以最大化，不显式请求激活。
                if (SendMessageTimeout(target.Window, 0x0112, 0xf030, 0, 0x0001 | 0x0002, 500, out _) == 0)
                    throw new DesktopException("DESKTOP_WINDOW_FAILED", "Windows did not acknowledge maximization.");
                break;
            case "minimize": ShowWindowAsync(target.Window, 7); break; // SW_SHOWMINNOACTIVE. 中文：SW_SHOWMINNOACTIVE。最小化窗口但不激活。
            case "restore":
                if (SendMessageTimeout(target.Window, 0x0112, 0xf120, 0, 0x0001 | 0x0002, 500, out _) == 0)
                    throw new DesktopException("DESKTOP_WINDOW_FAILED", "Windows did not acknowledge restoration.");
                if (SpinWait.SpinUntil(() => !IsIconic(target.Window), 300) && IsZoomed(target.Window) &&
                    SendMessageTimeout(target.Window, 0x0112, 0xf120, 0, 0x0001 | 0x0002, 500, out _) == 0)
                    throw new DesktopException("DESKTOP_WINDOW_FAILED", "Windows did not acknowledge restoring the normal size.");
                break;
            default: throw new DesktopException("DESKTOP_INVALID_REQUEST", "Unknown window mode.");
        }
        bool StateApplied() => request.Mode switch
        {
            "maximize" => IsZoomed(target.Window), "minimize" => IsIconic(target.Window),
            "restore" => !IsIconic(target.Window) && !IsZoomed(target.Window), _ => true
        };
        if (!SpinWait.SpinUntil(StateApplied, 500))
            throw new DesktopException("DESKTOP_WINDOW_FAILED", "The requested window state was not confirmed.");
        if (request.Mode != "minimize") placement.PlaceBehind(target.Window, target.ProcessId);
        target.Check(visible: false);
        GetClientRect(target.Window, out Rect size);
        return new() { ["windowId"] = target.Window.ToInt64().ToString(), ["processId"] = target.ProcessId,
            ["mode"] = request.Mode, ["clientWidth"] = size.Right, ["clientHeight"] = size.Bottom,
            ["isMinimized"] = IsIconic(target.Window), ["isMaximized"] = IsZoomed(target.Window),
            ["isForeground"] = GetForegroundWindow() == target.Window,
            ["foregroundPreserved"] = foregroundBefore == GetForegroundWindow(), ["activationRequested"] = false,
            ["backgroundPlacementConfirmed"] = placement.WindowPlacedBehind,
            ["focusRestoreAttempted"] = placement.FocusRestoreAttempted,
            ["focusRestoreSucceeded"] = placement.FocusRestoreSucceeded };
    }
}

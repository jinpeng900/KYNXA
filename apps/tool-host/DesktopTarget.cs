using System.Globalization;
using System.Runtime.InteropServices;
using System.Text;
using static KYNXA.ToolHost.DesktopNativeMethods;

namespace KYNXA.ToolHost;

internal sealed class DesktopTarget(nint window, int processId)
{
    internal nint Window { get; } = window;
    internal int ProcessId { get; } = processId;

    internal static bool IsInteractive()
    {
        if (!OperatingSystem.IsWindows() || !Environment.UserInteractive) return false;
        nint inputDesktopHandle = OpenInputDesktop(0, false, 0x101);
        if (inputDesktopHandle == 0) return false;
        try
        {
            string GetDesktopName(nint desktop)
            {
                var name = new StringBuilder(256);
                return GetUserObjectInformation(desktop, 2, name, 512, out _) ? name.ToString() : "";
            }
            string inputDesktopName = GetDesktopName(inputDesktopHandle), threadDesktopName = GetDesktopName(GetThreadDesktop(GetCurrentThreadId()));
            return inputDesktopName.Length > 0 && string.Equals(inputDesktopName, threadDesktopName, StringComparison.OrdinalIgnoreCase)
                && !string.Equals(inputDesktopName, "Winlogon", StringComparison.OrdinalIgnoreCase);
        }
        finally { CloseDesktop(inputDesktopHandle); }
    }

    internal static DesktopTarget From(DesktopRequest request)
    {
        if (!long.TryParse(request.WindowId, NumberStyles.None, CultureInfo.InvariantCulture, out long windowHandleValue)
            || windowHandleValue <= 0 || request.ProcessId is not > 0)
            throw new DesktopException("DESKTOP_INVALID_TARGET", "A windowId and matching processId are required.");
        var target = new DesktopTarget((nint)windowHandleValue, request.ProcessId.Value);
        target.Check(visible: request.Action is not "activate" and not "window");
        return target;
    }

    internal void Check(bool foreground = false, bool visible = true)
    {
        if (!IsInteractive()) throw new DesktopException("DESKTOP_UNAVAILABLE", "The interactive desktop is unavailable or locked.");
        if (!IsWindow(Window) || GetWindowThreadProcessId(Window, out uint ownerProcessId) == 0 || ownerProcessId != (uint)ProcessId
            || GetAncestor(Window, 2) != Window)
            throw new DesktopException("DESKTOP_TARGET_CHANGED", "The selected window no longer belongs to the selected process.");
        if (visible && (!IsWindowVisible(Window) || IsIconic(Window)))
            throw new DesktopException("DESKTOP_WINDOW_UNAVAILABLE", "The selected window is hidden or minimized.");
        if (foreground)
        {
            if (GetForegroundWindow() != Window)
                throw new DesktopException("DESKTOP_NOT_FOREGROUND", "Input requires the selected foreground window.");
            EnsureIntegrity();
        }
    }

    internal Rect ClientRect()
    {
        if (!GetClientRect(Window, out Rect rect) || rect.Right <= 0 || rect.Bottom <= 0)
            throw new DesktopException("DESKTOP_WINDOW_UNAVAILABLE", "The selected client area is unavailable.");
        return rect;
    }

    internal bool IsResponding() => !IsHungAppWindow(Window);

    internal void EnsureResponding()
    {
        Check(visible: false);
        // A bounded WM_NULL handshake avoids entering UIA for an already hung UI thread.
        // 使用有超时边界的 WM_NULL 握手，避免对已卡死的界面线程启动 UIA 读取。
        if (!IsResponding() || SendMessageTimeout(Window, 0, 0, 0, 0x0001 | 0x0002, 200, out _) == 0)
            throw new DesktopException("DESKTOP_NOT_RESPONDING", "The selected window is not responding; no UI Automation read was started.");
    }

    internal Point ClientPoint(int? x, int? y)
    {
        Check(foreground: true);
        Rect rect = ClientRect();
        if (x is null || y is null || x < 0 || y < 0 || x >= rect.Right || y >= rect.Bottom)
            throw new DesktopException("DESKTOP_INVALID_COORDINATES", "Coordinates must be inside the selected client area.");
        var point = new Point { X = x.Value, Y = y.Value };
        if (!ClientToScreen(Window, ref point) || GetAncestor(WindowFromPoint(point), 2) != Window)
            throw new DesktopException("DESKTOP_TARGET_OBSCURED", "The selected client point is obscured by another window.");
        return point;
    }

    internal DesktopWindow Describe()
    {
        var title = new StringBuilder(513);
        GetWindowText(Window, title, title.Capacity);
        string? path = ExecutablePath(ProcessId);
        string name = path is null ? "" : Path.GetFileNameWithoutExtension(path);
        GetClientRect(Window, out Rect rect);
        return new DesktopWindow(Window.ToInt64().ToString(CultureInfo.InvariantCulture), ProcessId, title.ToString(), name,
            path, rect.Right, rect.Bottom, GetForegroundWindow() == Window, IsIconic(Window), IsResponding());
    }

    internal static string? ExecutablePath(int processId)
    {
        nint processHandle = OpenProcess(ProcessQueryLimitedInformation, false, (uint)processId);
        if (processHandle == 0) return null;
        try
        {
            var text = new StringBuilder(4096); uint length = (uint)text.Capacity;
            return QueryFullProcessImageName(processHandle, 0, text, ref length) ? text.ToString() : null;
        }
        finally { CloseHandle(processHandle); }
    }

    internal void EnsureIntegrity()
    {
        nint processHandle = OpenProcess(ProcessQueryLimitedInformation, false, (uint)ProcessId);
        if (processHandle == 0) throw new DesktopException("DESKTOP_ACCESS_DENIED", "The selected process cannot be queried; no elevation was attempted.");
        try
        {
            if (GetIntegrityLevel(processHandle) > GetIntegrityLevel(GetCurrentProcess()))
                throw new DesktopException("DESKTOP_UIPI_BLOCKED", "The selected process has higher integrity; no elevation was attempted.");
        }
        finally { CloseHandle(processHandle); }
    }

    private static int GetIntegrityLevel(nint processHandle)
    {
        if (!OpenProcessToken(processHandle, TokenQuery, out nint tokenHandle))
            throw new DesktopException("DESKTOP_ACCESS_DENIED", "Process integrity cannot be verified; no elevation was attempted.");
        try
        {
            GetTokenInformation(tokenHandle, TokenIntegrityLevel, 0, 0, out uint length);
            if (length == 0 || length > 65536) throw new DesktopException("DESKTOP_ACCESS_DENIED", "Process integrity cannot be verified.");
            nint integrityBuffer = Marshal.AllocHGlobal((int)length);
            try
            {
                if (!GetTokenInformation(tokenHandle, TokenIntegrityLevel, integrityBuffer, length, out _))
                    throw new DesktopException("DESKTOP_ACCESS_DENIED", "Process integrity cannot be verified.");
                nint sid = Marshal.ReadIntPtr(integrityBuffer);
                byte subAuthorityCount = Marshal.ReadByte(GetSidSubAuthorityCount(sid));
                if (subAuthorityCount == 0) throw new DesktopException("DESKTOP_ACCESS_DENIED", "Process integrity cannot be verified.");
                return Marshal.ReadInt32(GetSidSubAuthority(sid, (uint)(subAuthorityCount - 1)));
            }
            finally { Marshal.FreeHGlobal(integrityBuffer); }
        }
        finally { CloseHandle(tokenHandle); }
    }
}

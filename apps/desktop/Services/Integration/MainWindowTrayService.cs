using System.Diagnostics;
using System.Runtime.InteropServices;
using Microsoft.UI.Xaml;

namespace KYNXA_Desktop.Services;

/// <summary>
/// Keeps the main window and its running work alive when it is closed to the notification area.
/// 主窗口关闭到通知区域时，保留同一个窗口及其正在执行的工作。
/// </summary>
internal sealed class MainWindowTrayService : IDisposable
{
    private const uint CloseMessage = 0x0010;
    private const uint QueryEndSessionMessage = 0x0011;
    private const uint EndSessionMessage = 0x0016;
    private const uint ShowWindowMessage = 0x0018;
    private const uint NonClientDestroyMessage = 0x0082;
    private const uint TrayCallbackMessage = 0x8000 + 73;
    private const nuint SubclassId = 0x4B595452;
    private const uint TrayIconId = 1;
    private const uint OpenMenuId = 1;
    private const uint ExitMenuId = 2;
    private readonly Window _window;
    private readonly nint _windowHandle;
    private readonly WindowSubclassProc _windowProcedure;
    private readonly uint _taskbarCreatedMessage;
    private nint _icon;
    private bool _ownsIcon;
    private bool _isSubclassAttached;
    private bool _isIconRegistered;
    private bool _exitRequested;
    private bool _sessionEnding;
    private bool _isDisposed;

    public MainWindowTrayService(Window window)
    {
        _window = window;
        _windowHandle = WinRT.Interop.WindowNative.GetWindowHandle(window);
        _windowProcedure = WindowProcedure;
        _taskbarCreatedMessage = RegisterWindowMessage("TaskbarCreated");
        _icon = LoadImage(0, Path.Combine(AppContext.BaseDirectory, "Assets", "AppIcon.ico"), 1, 0, 0, 0x0010 | 0x0040);
        _ownsIcon = _icon != 0;
        if (_icon == 0) _icon = LoadIcon(0, (nint)32512);
        _isSubclassAttached = _icon != 0 && SetWindowSubclass(_windowHandle, _windowProcedure, SubclassId, 0);
        window.Closed += WindowClosed;
    }

    public void Restore()
    {
        if (_isDisposed) return;
        // Restore the existing shell, including its draft, streaming replies and workspace instances.
        // 恢复原有界面，保留草稿、流式回复及工作区实例，不重新创建窗口。
        ShowWindow(_windowHandle, IsIconic(_windowHandle) ? 9 : 5);
        _window.Activate();
        SetForegroundWindow(_windowHandle);
    }

    public void RequestExit()
    {
        if (_isDisposed || _sessionEnding) return;
        if (!IsWindowVisible(_windowHandle)) Restore();
        _exitRequested = true;
        // Reuse the existing cancellable save/cleanup flow; never terminate the shared gateway.
        // 复用现有可取消的保存与清理流程，不终止共享网关。
        SendMessage(_windowHandle, CloseMessage, 0, 0);
    }

    private nint WindowProcedure(nint window, uint message, nint wParam, nint lParam, nuint subclassId, nuint referenceData)
    {
        try
        {
            if (message == QueryEndSessionMessage) _sessionEnding = true;
            else if (message == EndSessionMessage && wParam == 0) _sessionEnding = false;
            else if (message == ShowWindowMessage && wParam != 0 && !_sessionEnding) _exitRequested = false;
            else if (message == CloseMessage && !_sessionEnding && !_exitRequested && EnsureTrayIcon())
            {
                // Intercept before AppWindow.Closing: a tray close must not pause replies or save as an exit.
                // 在 AppWindow.Closing 之前拦截；关闭到托盘不能触发暂停回复或退出保存。
                _window.AppWindow.Hide();
                return 0;
            }
            else if (message == TrayCallbackMessage)
            {
                uint notification = (uint)(lParam.ToInt64() & 0xffff);
                if (notification is 0x0400 or 0x0401 or 0x0203) Restore();
                else if (notification is 0x007B or 0x0205) ShowMenu();
                return 0;
            }
            else if (_taskbarCreatedMessage != 0 && message == _taskbarCreatedMessage && _isIconRegistered)
            {
                _isIconRegistered = false;
                // Re-register the tray icon after Explorer restarts; restore the window if it fails.
                // 资源管理器重启后重新注册托盘图标，失败则显示窗口，避免无法找回。
                if (!EnsureTrayIcon() && !IsWindowVisible(_windowHandle)) Restore();
            }
            else if (message == NonClientDestroyMessage) Dispose();
        }
        catch (Exception error)
        {
            // Exceptions must not cross the native callback boundary or swallow the normal close fallback.
            // 异常不能越过原生回调边界，也不能阻断普通关闭的回退路径。
            Debug.WriteLine($"Notification-area window lifecycle: {error.GetType().Name}");
        }

        nint result = DefSubclassProc(window, message, wParam, lParam);
        // A synchronous exit veto keeps the window usable and close-to-tray enabled.
        // 数据迁移等操作拒绝退出后，窗口仍可用，下次点 X 仍关闭到托盘。
        if (message == CloseMessage && _exitRequested && !_isDisposed && IsWindowVisible(window)) _exitRequested = false;
        return result;
    }

    private bool EnsureTrayIcon()
    {
        if (_isIconRegistered) return true;
        if (!_isSubclassAttached || _isDisposed) return false;
        var data = CreateIconData();
        if (!ShellNotifyIcon(0, ref data)) return false;
        data.TimeoutOrVersion = 4;
        if (!ShellNotifyIcon(4, ref data))
        {
            ShellNotifyIcon(2, ref data);
            return false;
        }
        _isIconRegistered = true;
        return true;
    }

    private NotifyIconData CreateIconData() => new()
    {
        Size = (uint)Marshal.SizeOf<NotifyIconData>(),
        Window = _windowHandle,
        Id = TrayIconId,
        Flags = 0x0001 | 0x0002 | 0x0004 | 0x0080,
        CallbackMessage = TrayCallbackMessage,
        Icon = _icon,
        Tip = "KYNXA",
        Info = string.Empty,
        InfoTitle = string.Empty
    };

    private void ShowMenu()
    {
        nint menu = CreatePopupMenu();
        if (menu == 0) { Restore(); return; }
        try
        {
            AppendMenu(menu, 0, OpenMenuId, UiText.Get("打开 KYNXA"));
            AppendMenu(menu, 0x0800, 0, null);
            AppendMenu(menu, 0, ExitMenuId, UiText.Get("退出 KYNXA"));
            SetMenuDefaultItem(menu, OpenMenuId, false);
            GetCursorPos(out var point);
            SetForegroundWindow(_windowHandle);
            uint command = TrackPopupMenu(menu, 0x0100 | 0x0002, point.X, point.Y, 0, _windowHandle, 0);
            PostMessage(_windowHandle, 0, 0, 0);
            if (command == OpenMenuId) Restore();
            else if (command == ExitMenuId) RequestExit();
        }
        finally { DestroyMenu(menu); }
    }

    private void WindowClosed(object sender, WindowEventArgs args) => Dispose();

    public void Dispose()
    {
        if (_isDisposed) return;
        _isDisposed = true;
        _window.Closed -= WindowClosed;
        if (_isIconRegistered)
        {
            var data = CreateIconData();
            ShellNotifyIcon(2, ref data);
            _isIconRegistered = false;
        }
        if (_isSubclassAttached)
        {
            RemoveWindowSubclass(_windowHandle, _windowProcedure, SubclassId);
            _isSubclassAttached = false;
        }
        if (_ownsIcon && _icon != 0) DestroyIcon(_icon);
        _icon = 0;
    }

    [StructLayout(LayoutKind.Sequential, CharSet = CharSet.Unicode)]
    private struct NotifyIconData
    {
        public uint Size;
        public nint Window;
        public uint Id;
        public uint Flags;
        public uint CallbackMessage;
        public nint Icon;
        [MarshalAs(UnmanagedType.ByValTStr, SizeConst = 128)] public string Tip;
        public uint State;
        public uint StateMask;
        [MarshalAs(UnmanagedType.ByValTStr, SizeConst = 256)] public string Info;
        public uint TimeoutOrVersion;
        [MarshalAs(UnmanagedType.ByValTStr, SizeConst = 64)] public string InfoTitle;
        public uint InfoFlags;
        public Guid ItemGuid;
        public nint BalloonIcon;
    }

    [StructLayout(LayoutKind.Sequential)]
    private struct NativePoint { public int X; public int Y; }

    private delegate nint WindowSubclassProc(nint window, uint message, nint wParam, nint lParam, nuint subclassId, nuint referenceData);

    [DllImport("comctl32.dll", ExactSpelling = true)]
    [return: MarshalAs(UnmanagedType.Bool)]
    private static extern bool SetWindowSubclass(nint window, WindowSubclassProc callback, nuint subclassId, nuint referenceData);
    [DllImport("comctl32.dll", ExactSpelling = true)]
    [return: MarshalAs(UnmanagedType.Bool)]
    private static extern bool RemoveWindowSubclass(nint window, WindowSubclassProc callback, nuint subclassId);
    [DllImport("comctl32.dll", ExactSpelling = true)]
    private static extern nint DefSubclassProc(nint window, uint message, nint wParam, nint lParam);
    [DllImport("shell32.dll", EntryPoint = "Shell_NotifyIconW", CharSet = CharSet.Unicode)]
    [return: MarshalAs(UnmanagedType.Bool)]
    private static extern bool ShellNotifyIcon(uint message, ref NotifyIconData data);
    [DllImport("user32.dll", EntryPoint = "RegisterWindowMessageW", CharSet = CharSet.Unicode)]
    private static extern uint RegisterWindowMessage(string message);
    [DllImport("user32.dll", EntryPoint = "LoadImageW", CharSet = CharSet.Unicode)]
    private static extern nint LoadImage(nint instance, string name, uint type, int width, int height, uint flags);
    [DllImport("user32.dll", EntryPoint = "LoadIconW")]
    private static extern nint LoadIcon(nint instance, nint icon);
    [DllImport("user32.dll")]
    [return: MarshalAs(UnmanagedType.Bool)]
    private static extern bool DestroyIcon(nint icon);
    [DllImport("user32.dll")]
    [return: MarshalAs(UnmanagedType.Bool)]
    private static extern bool ShowWindow(nint window, int command);
    [DllImport("user32.dll")]
    [return: MarshalAs(UnmanagedType.Bool)]
    private static extern bool IsIconic(nint window);
    [DllImport("user32.dll")]
    [return: MarshalAs(UnmanagedType.Bool)]
    private static extern bool IsWindowVisible(nint window);
    [DllImport("user32.dll")]
    [return: MarshalAs(UnmanagedType.Bool)]
    private static extern bool SetForegroundWindow(nint window);
    [DllImport("user32.dll", EntryPoint = "PostMessageW")]
    [return: MarshalAs(UnmanagedType.Bool)]
    private static extern bool PostMessage(nint window, uint message, nint wParam, nint lParam);
    [DllImport("user32.dll", EntryPoint = "SendMessageW")]
    private static extern nint SendMessage(nint window, uint message, nint wParam, nint lParam);
    [DllImport("user32.dll")]
    private static extern nint CreatePopupMenu();
    [DllImport("user32.dll", EntryPoint = "AppendMenuW", CharSet = CharSet.Unicode)]
    [return: MarshalAs(UnmanagedType.Bool)]
    private static extern bool AppendMenu(nint menu, uint flags, nuint item, string? text);
    [DllImport("user32.dll")]
    [return: MarshalAs(UnmanagedType.Bool)]
    private static extern bool SetMenuDefaultItem(nint menu, uint item, [MarshalAs(UnmanagedType.Bool)] bool byPosition);
    [DllImport("user32.dll")]
    [return: MarshalAs(UnmanagedType.Bool)]
    private static extern bool GetCursorPos(out NativePoint point);
    [DllImport("user32.dll")]
    private static extern uint TrackPopupMenu(nint menu, uint flags, int x, int y, int reserved, nint window, nint rectangle);
    [DllImport("user32.dll")]
    [return: MarshalAs(UnmanagedType.Bool)]
    private static extern bool DestroyMenu(nint menu);
}

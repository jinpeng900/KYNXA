using System.ComponentModel;
using System.Runtime.InteropServices;
using System.Text;
using static KYNXA.ToolHost.NativeMethods;

namespace KYNXA.ToolHost;

/// <summary>Real console verification and bounded screen reading, never a redirected-output TTY simulation.</summary>
/// <remarks>验证真实控制台并有界读取屏幕，不用重定向输出模拟交互终端。</remarks>
internal static class HostTerminalConsole
{
    internal const int MaximumSnapshotCharacters = 65536;

    internal static void Check(bool succeeded, string operation)
    {
        if (!succeeded)
            throw new HostTerminalException("HOST_TERMINAL_START_FAILED", $"{operation}: {new Win32Exception(Marshal.GetLastWin32Error()).Message}");
    }

    internal static bool HasRealConsoleHandles() =>
        GetConsoleMode(GetStdHandle(-10), out _) && GetConsoleMode(GetStdHandle(-11), out _);

    internal static IntPtr FindVisibleWindow(string title)
    {
        IntPtr ownConsole = GetConsoleWindow();
        if (IsVisible(ownConsole)) return ownConsole;
        // Windows Terminal delegation exposes a message-only GetConsoleWindow. A unique title proves
        // which real host window displays this console; an unrelated terminal is never accepted.
        // Windows Terminal 托管时 GetConsoleWindow 可能只返回消息窗口；通过唯一标题确认真实宿主窗口，不接受无关终端。
        IntPtr result = IntPtr.Zero;
        EnumWindows((window, _) =>
        {
            if (!IsVisible(window)) return true;
            var className = new StringBuilder(128);
            GetClassNameW(window, className, className.Capacity);
            if (className.ToString() is not ("ConsoleWindowClass" or "CASCADIA_HOSTING_WINDOW_CLASS")) return true;
            var name = new StringBuilder(1024);
            GetWindowTextW(window, name, name.Capacity);
            if (!name.ToString().Contains(title, StringComparison.Ordinal)) return true;
            result = window;
            return false;
        }, IntPtr.Zero);
        return result;
    }

    internal static bool IsVisible(IntPtr window) => window != IntPtr.Zero && IsWindowVisible(window) && !IsIconic(window) &&
        GetClientRect(window, out WindowRectangle rectangle) && rectangle.Right > 0 && rectangle.Bottom > 0;
    internal static int WindowProcessId(IntPtr window)
    {
        GetWindowThreadProcessId(window, out uint owner);
        return checked((int)owner);
    }

    internal static (string Text, bool Truncated, bool Available) Snapshot()
    {
        IntPtr output = GetStdHandle(-11);
        if (!GetConsoleScreenBufferInfo(output, out ConsoleScreenBufferInfo information)) return ("", false, false);
        int columns = information.Size.X;
        if (columns <= 0) return ("", false, false);
        int endRow = Math.Clamp((int)information.CursorPosition.Y + 1, 1, information.Size.Y);
        int maximumRows = Math.Max(1, MaximumSnapshotCharacters / columns);
        int startRow = Math.Max(0, endRow - maximumRows);
        int characters = Math.Min(MaximumSnapshotCharacters, columns * (endRow - startRow));
        var buffer = new StringBuilder(characters);
        if (!ReadConsoleOutputCharacterW(output, buffer, (uint)characters, new Coordinate(0, (short)startRow), out uint read))
            return ("", false, false);
        string raw = buffer.ToString();
        var lines = new List<string>();
        for (int offset = 0; offset < Math.Min(raw.Length, (int)read); offset += columns)
            lines.Add(raw.Substring(offset, Math.Min(columns, raw.Length - offset)).TrimEnd(' ', '\0'));
        string text = string.Join('\n', lines).TrimEnd('\n');
        // A delegated Terminal can expose only its current screen rather than historical scrollback.
        // A cursor at the buffer bottom therefore conservatively marks possible prior-row loss.
        // 托管终端可能只暴露当前屏幕，不提供历史回滚；光标位于缓冲区底部时保守标记此前行可能丢失。
        bool truncated = startRow != 0 || information.CursorPosition.Y >= information.Size.Y - 1;
        if (text.Length > MaximumSnapshotCharacters)
        {
            int offset = text.Length - MaximumSnapshotCharacters;
            if (offset > 0 && char.IsLowSurrogate(text[offset]) && char.IsHighSurrogate(text[offset - 1])) offset++;
            text = text[offset..];
            truncated = true;
        }
        return (text, truncated, true);
    }

    internal static void SetTitle(string title) => SetConsoleTitleW(title);

    [StructLayout(LayoutKind.Sequential)]
    private struct Coordinate(short x, short y)
    {
        internal short X = x, Y = y;
    }

    [StructLayout(LayoutKind.Sequential)]
    private struct Rectangle { internal short Left, Top, Right, Bottom; }

    [StructLayout(LayoutKind.Sequential)]
    private struct WindowRectangle { internal int Left, Top, Right, Bottom; }

    [StructLayout(LayoutKind.Sequential)]
    private struct ConsoleScreenBufferInfo
    {
        internal Coordinate Size, CursorPosition;
        internal ushort Attributes;
        internal Rectangle Window;
        internal Coordinate MaximumWindowSize;
    }

    private delegate bool WindowCallback(IntPtr window, IntPtr state);
    [DllImport("kernel32.dll")] private static extern IntPtr GetStdHandle(int id);
    [DllImport("kernel32.dll")] private static extern IntPtr GetConsoleWindow();
    [DllImport("kernel32.dll")] private static extern bool GetConsoleMode(IntPtr handle, out uint mode);
    [DllImport("kernel32.dll", CharSet = CharSet.Unicode)] private static extern bool SetConsoleTitleW(string title);
    [DllImport("kernel32.dll")] private static extern bool GetConsoleScreenBufferInfo(IntPtr handle, out ConsoleScreenBufferInfo information);
    [DllImport("kernel32.dll", CharSet = CharSet.Unicode)] private static extern bool ReadConsoleOutputCharacterW(IntPtr handle, StringBuilder text, uint count, Coordinate position, out uint read);
    [DllImport("user32.dll")] private static extern bool EnumWindows(WindowCallback callback, IntPtr state);
    [DllImport("user32.dll")] private static extern bool IsWindowVisible(IntPtr window);
    [DllImport("user32.dll")] private static extern bool IsIconic(IntPtr window);
    [DllImport("user32.dll")] private static extern bool GetClientRect(IntPtr window, out WindowRectangle rectangle);
    [DllImport("user32.dll")] private static extern uint GetWindowThreadProcessId(IntPtr window, out uint processId);
    [DllImport("user32.dll", CharSet = CharSet.Unicode)] private static extern int GetClassNameW(IntPtr window, StringBuilder name, int capacity);
    [DllImport("user32.dll", CharSet = CharSet.Unicode)] private static extern int GetWindowTextW(IntPtr window, StringBuilder text, int capacity);
}

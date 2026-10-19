using System.Runtime.InteropServices;
using System.Windows;
using System.Windows.Interop;
using System.Windows.Media;

namespace Kynxa.UsageMonitor;

/// <summary>
/// Uses physical screen coordinates for saved placement and drag deltas across monitors with different DPI.
/// 位置存档与拖动差值使用物理屏幕坐标，避免不同 DPI 显示器之间出现偏移。
/// </summary>
internal static class WindowPlacement
{
    public static Point CursorPosition()
    {
        GetCursorPos(out NativePoint point);
        return new Point(point.X, point.Y);
    }

    public static Rect Bounds(Window window)
    {
        GetWindowRect(new WindowInteropHelper(window).Handle, out NativeRect rectangle);
        return new Rect(rectangle.Left, rectangle.Top, rectangle.Right - rectangle.Left, rectangle.Bottom - rectangle.Top);
    }

    public static void Restore(Window window, double? savedLeft, double? savedTop)
    {
        Rect bounds = Bounds(window);
        if (savedLeft.HasValue && savedTop.HasValue)
        {
            MoveClamped(window, savedLeft.Value, savedTop.Value, new Point(savedLeft.Value + bounds.Width / 2, savedTop.Value + bounds.Height / 2));
        }
        else
        {
            MoveToBottomRight(window);
        }
    }

    public static void MoveToBottomRight(Window window)
    {
        IntPtr monitor = MonitorFromPoint(new NativePoint(), 1);
        MonitorInformation information = new() { Size = Marshal.SizeOf<MonitorInformation>() };
        GetMonitorInfo(monitor, ref information);
        Rect bounds = Bounds(window);
        double margin = 24 * VisualTreeHelper.GetDpi(window).DpiScaleX;
        SetPosition(window, information.Work.Right - bounds.Width - margin, information.Work.Bottom - bounds.Height - margin);
    }

    public static void MoveClamped(Window window, double left, double top, Point referencePoint)
    {
        NativePoint point = new() { X = (int)Math.Round(referencePoint.X), Y = (int)Math.Round(referencePoint.Y) };
        IntPtr monitor = MonitorFromPoint(point, 2);
        MonitorInformation information = new() { Size = Marshal.SizeOf<MonitorInformation>() };
        if (!GetMonitorInfo(monitor, ref information)) return;
        Rect bounds = Bounds(window);
        left = Math.Clamp(left, information.Work.Left, Math.Max(information.Work.Left, information.Work.Right - bounds.Width));
        top = Math.Clamp(top, information.Work.Top, Math.Max(information.Work.Top, information.Work.Bottom - bounds.Height));
        SetPosition(window, left, top);
    }

    private static void SetPosition(Window window, double left, double top)
    {
        SetWindowPos(new WindowInteropHelper(window).Handle, IntPtr.Zero, (int)Math.Round(left), (int)Math.Round(top), 0, 0, 0x0001 | 0x0004 | 0x0010);
    }

    [StructLayout(LayoutKind.Sequential)] private struct NativePoint { public int X; public int Y; }
    [StructLayout(LayoutKind.Sequential)] private struct NativeRect { public int Left; public int Top; public int Right; public int Bottom; }
    [StructLayout(LayoutKind.Sequential)] private struct MonitorInformation { public int Size; public NativeRect Monitor; public NativeRect Work; public uint Flags; }
    [DllImport("user32.dll")] [return: MarshalAs(UnmanagedType.Bool)] private static extern bool GetCursorPos(out NativePoint point);
    [DllImport("user32.dll")] private static extern IntPtr MonitorFromPoint(NativePoint point, uint flags);
    [DllImport("user32.dll", CharSet = CharSet.Unicode)] [return: MarshalAs(UnmanagedType.Bool)] private static extern bool GetMonitorInfo(IntPtr monitor, ref MonitorInformation information);
    [DllImport("user32.dll")] [return: MarshalAs(UnmanagedType.Bool)] private static extern bool GetWindowRect(IntPtr window, out NativeRect rectangle);
    [DllImport("user32.dll")] [return: MarshalAs(UnmanagedType.Bool)] private static extern bool SetWindowPos(IntPtr window, IntPtr insertAfter, int x, int y, int width, int height, uint flags);
}

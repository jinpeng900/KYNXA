using System.IO;
using System.Runtime.InteropServices;
using System.Text.Json;

namespace MemoryUiSmoke;

/// <summary>
/// Activates only this process's fixture through ordinary Windows calls or a verified real caption click.
/// 仅通过普通 Windows 调用或核实归属后的真实标题栏点击激活本进程夹具。
/// </summary>
internal static class OwnedWindowActivation
{
    public static async Task EnsureAsync(nint owned, Action raiseOwnWindow, Action restoreOwnWindow, string? diagnosticPath)
    {
        WindowProcess(owned, out uint process);
        if (process != Environment.ProcessId || !IsVisible(owned) || IsIconic(owned))
            throw new InvalidOperationException("Activation requires this process's visible, non-minimized fixture window.");
        SetForegroundWindow(owned);
        await Task.Delay(150);
        Record("ordinary-activation", owned, diagnosticPath);
        if (GetForegroundWindow() == owned) return;
        if (!GetCursorPos(out var originalPointer)) throw new InvalidOperationException("Cannot read the original pointer position.");
        EnsureNoHeldInput();
        try
        {
            // Raise only the owned fixture, and restore its original Z-order policy even if the hit test fails.
            // 只提升自有夹具，命中检查失败时也还原原有置顶设置。
            raiseOwnWindow();
            await Task.Delay(150);
            var point = CaptionPoint(owned);
            Record("before-owned-title-click", owned, diagnosticPath, point);
            EnsureOwnedPoint(owned, point);
            int left = GetSystemMetrics(76), top = GetSystemMetrics(77), width = GetSystemMetrics(78), height = GetSystemMetrics(79);
            if (width <= 1 || height <= 1 || point.X < left || point.Y < top || point.X >= left + width || point.Y >= top + height)
                throw new InvalidOperationException("The owned fixture caption is outside the physical virtual screen.");
            SendMouse([new Input { Data = new InputData { Mouse = new MouseInput {
                X = (int)Math.Round((point.X - left) * 65535d / (width - 1)),
                Y = (int)Math.Round((point.Y - top) * 65535d / (height - 1)), Flags = 0x8000 | 0x4000 | 0x0001 } } }]);
            await Task.Delay(40);
            if (!GetCursorPos(out var actual) || Math.Abs(actual.X - point.X) > 1 || Math.Abs(actual.Y - point.Y) > 1)
                throw new InvalidOperationException("The owned fixture activation point moved before clicking.");
            EnsureOwnedPoint(owned, actual);
            EnsureNoHeldInput();
            SendMouse([new Input { Data = new InputData { Mouse = new MouseInput { Flags = 0x0002 } } },
                new Input { Data = new InputData { Mouse = new MouseInput { Flags = 0x0004 } } }]);
            for (int attempt = 0; attempt < 80 && GetForegroundWindow() != owned; attempt++) await Task.Delay(25);
            if (GetForegroundWindow() != owned) throw new InvalidOperationException("The real owned-window caption click did not acquire foreground.");
        }
        finally
        {
            try { Record("activation-finished", owned, diagnosticPath); }
            finally
            {
                try { restoreOwnWindow(); }
                finally { if (GetForegroundWindow() == owned) SetCursorPos(originalPointer.X, originalPointer.Y); }
            }
        }
    }

    private static Point CaptionPoint(nint owned)
    {
        if (!GetWindowRect(owned, out var bounds) || !GetClientRect(owned, out var client) || client.Right <= 0 || client.Bottom <= 0)
            throw new InvalidOperationException("Cannot read the owned fixture's actual native bounds.");
        var origin = new Point();
        if (!ClientToScreen(owned, ref origin)) throw new InvalidOperationException("Cannot locate the owned fixture client origin.");
        double scale = GetDpiForWindow(owned) / 96d;
        if (origin.Y - bounds.Top > 8 * scale)
            return new Point { X = bounds.Left + (bounds.Right - bounds.Left) / 2, Y = bounds.Top + (origin.Y - bounds.Top) / 2 };
        // WinUI extends a 48-DIP caption into its client area; use actual client pixels rather than cached XAML width.
        // WinUI 将 48 DIP 标题栏延伸进客户区，使用真实客户区像素而非缓存的 XAML 宽度。
        var point = new Point { X = (int)Math.Round(client.Right * 0.6), Y = Math.Min(client.Bottom - 1, (int)Math.Round(24 * scale)) };
        if (!ClientToScreen(owned, ref point)) throw new InvalidOperationException("Cannot map the owned caption to the physical desktop.");
        return point;
    }

    private static void EnsureOwnedPoint(nint owned, Point point)
    {
        WindowProcess(owned, out uint process);
        if (process != Environment.ProcessId || !IsVisible(owned) || IsIconic(owned) || GetAncestor(WindowFromPoint(point), 2) != owned)
            throw new InvalidOperationException("The fixture's blank title bar is covered or outside the reachable screen.");
    }

    private static void EnsureNoHeldInput()
    {
        foreach (int key in new[] { 0x01, 0x02, 0x04, 0x10, 0x11, 0x12, 0x5B, 0x5C })
            if ((GetAsyncKeyState(key) & 0x8000) != 0)
                throw new InvalidOperationException("A held mouse button or modifier prevents isolated fixture activation.");
    }

    private static void Record(string stage, nint owned, string? path, Point? point = null)
    {
        if (path is null) return;
        GetWindowRect(owned, out var bounds);
        GetClientRect(owned, out var client);
        var foreground = GetForegroundWindow();
        WindowProcess(foreground, out uint foregroundProcess);
        File.AppendAllText(path, JsonSerializer.Serialize(new {
            stage, owned = owned.ToInt64(), ownedProcess = Environment.ProcessId,
            foreground = foreground.ToInt64(), foregroundProcess,
            visible = IsVisible(owned), minimized = IsIconic(owned), dpi = GetDpiForWindow(owned),
            bounds = new { bounds.Left, bounds.Top, bounds.Right, bounds.Bottom },
            client = new { client.Left, client.Top, client.Right, client.Bottom },
            point = point is { } p ? new { p.X, p.Y } : null
        }) + Environment.NewLine);
    }

    private static void SendMouse(Input[] inputs)
    {
        uint sent = SendInput((uint)inputs.Length, inputs, Marshal.SizeOf<Input>());
        if (sent == inputs.Length) return;
        int error = Marshal.GetLastWin32Error();
        // If only down was accepted, release it once without replaying the click or reporting success.
        // 若仅按下已送达，只补发一次松开，不重做点击，也不宣称成功。
        if (inputs.Length == 2 && sent == 1 && inputs[0].Data.Mouse.Flags == 0x0002 && inputs[1].Data.Mouse.Flags == 0x0004)
        {
            uint released = SendInput(1, [inputs[1]], Marshal.SizeOf<Input>());
            throw new InvalidOperationException($"Owned activation sent only mouse-down; cleanup released {released}/1 event; Win32={error}.");
        }
        throw new InvalidOperationException($"Owned activation sent {sent}/{inputs.Length} events; Win32={error}.");
    }

    [StructLayout(LayoutKind.Sequential)] private struct Point { public int X, Y; }
    [StructLayout(LayoutKind.Sequential)] private struct Rect { public int Left, Top, Right, Bottom; }
    [StructLayout(LayoutKind.Sequential)] private struct Input { public uint Type; public InputData Data; }
    [StructLayout(LayoutKind.Explicit)] private struct InputData { [FieldOffset(0)] public MouseInput Mouse; }
    [StructLayout(LayoutKind.Sequential)] private struct MouseInput { public int X, Y; public uint MouseData, Flags, Time; public nuint ExtraInfo; }
    [DllImport("user32.dll", EntryPoint = "GetWindowThreadProcessId")] private static extern uint WindowProcess(nint window, out uint process);
    [DllImport("user32.dll", EntryPoint = "IsWindowVisible")] private static extern bool IsVisible(nint window);
    [DllImport("user32.dll")] private static extern bool IsIconic(nint window);
    [DllImport("user32.dll")] private static extern bool SetForegroundWindow(nint window);
    [DllImport("user32.dll")] private static extern nint GetForegroundWindow();
    [DllImport("user32.dll")] private static extern uint GetDpiForWindow(nint window);
    [DllImport("user32.dll")] private static extern bool GetWindowRect(nint window, out Rect bounds);
    [DllImport("user32.dll")] private static extern bool GetClientRect(nint window, out Rect bounds);
    [DllImport("user32.dll")] private static extern bool ClientToScreen(nint window, ref Point point);
    [DllImport("user32.dll")] private static extern bool GetCursorPos(out Point point);
    [DllImport("user32.dll")] private static extern bool SetCursorPos(int x, int y);
    [DllImport("user32.dll")] private static extern short GetAsyncKeyState(int key);
    [DllImport("user32.dll")] private static extern int GetSystemMetrics(int index);
    [DllImport("user32.dll")] private static extern nint WindowFromPoint(Point point);
    [DllImport("user32.dll")] private static extern nint GetAncestor(nint window, uint flags);
    [DllImport("user32.dll", SetLastError = true)] private static extern uint SendInput(uint count, Input[] inputs, int size);
}

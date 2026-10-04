using System.Runtime.InteropServices;
using KYNXA_Desktop.Views;
using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Controls;

namespace ScreenshotPanelUiSmoke;

/// <summary>Finite physical inputs restricted to this fixture's foreground, synthetic full-screen window.
/// 有限的真实输入仅作用于此夹具前台的虚构全屏窗口。
/// </summary>
internal sealed class ScreenshotFixtureInput : IDisposable
{
    private readonly nint _window;
    private readonly ScreenPoint _original;
    public bool UsesPhysicalInput { get; }

    public ScreenshotFixtureInput(ScreenshotViewerWindow viewer)
    {
        _window = WinRT.Interop.WindowNative.GetWindowHandle(viewer);
        GetCursorPos(out _original);
        SetForegroundWindow(_window);
        UsesPhysicalInput = GetForegroundWindow() == _window;
    }

    public void Wheel(Grid root, ScrollViewer viewport, int delta)
    {
        Position(root, viewport, 0, 0); SendMouse(0x0800, unchecked((uint)delta));
    }

    public async Task DragAsync(Grid root, ScrollViewer viewport, int dx, int dy)
    {
        Position(root, viewport, 0, 0); SendMouse(0x0002, 0);
        await Task.Delay(40);
        Position(root, viewport, dx, dy);
        await Task.Delay(40);
        SendMouse(0x0004, 0);
        await Task.Delay(40);
    }

    public void Move(Grid root, ScrollViewer viewport, int dx, int dy)
    { Position(root, viewport, dx, dy); }

    public void Escape()
    {
        RequireTarget();
        var keys = new[] { new Input { Type = 1, Data = new InputData { Keyboard = new KeyboardInput { VirtualKey = 0x1B } } },
            new Input { Type = 1, Data = new InputData { Keyboard = new KeyboardInput { VirtualKey = 0x1B, Flags = 2 } } } };
        Send(keys);
    }

    private void Position(Grid root, ScrollViewer viewport, int dx, int dy)
    {
        RequireTarget();
        var point = viewport.TransformToVisual(root).TransformPoint(new Windows.Foundation.Point(viewport.ActualWidth / 2, viewport.ActualHeight / 2));
        var screen = new ScreenPoint { X = (int)(point.X * root.XamlRoot.RasterizationScale) + dx,
            Y = (int)(point.Y * root.XamlRoot.RasterizationScale) + dy };
        ClientToScreen(_window, ref screen);
        SetCursorPos(screen.X, screen.Y);
    }

    private void SendMouse(uint flags, uint data)
    { RequireTarget(); Send([new Input { Data = new InputData { Mouse = new MouseInput { Flags = flags, Data = data } } }]); }
    private static void Send(Input[] values)
    { if (SendInput((uint)values.Length, values, Marshal.SizeOf<Input>()) != values.Length) throw new InvalidOperationException("Fixture native input failed."); }
    private void RequireTarget()
    {
        GetWindowThreadProcessId(_window, out uint process);
        if (!UsesPhysicalInput || process != Environment.ProcessId || GetForegroundWindow() != _window)
            throw new InvalidOperationException("Native inputs refused outside the owned screenshot fixture.");
    }

    public void Dispose() { if (UsesPhysicalInput) SetCursorPos(_original.X, _original.Y); }

    [StructLayout(LayoutKind.Sequential)] private struct ScreenPoint { public int X, Y; }
    [StructLayout(LayoutKind.Sequential)] private struct Input { public uint Type; public InputData Data; }
    [StructLayout(LayoutKind.Explicit)] private struct InputData
    { [FieldOffset(0)] public MouseInput Mouse; [FieldOffset(0)] public KeyboardInput Keyboard; }
    [StructLayout(LayoutKind.Sequential)] private struct MouseInput
    { public int X, Y; public uint Data, Flags, Time; public nuint ExtraInfo; }
    [StructLayout(LayoutKind.Sequential)] private struct KeyboardInput
    { public ushort VirtualKey, ScanCode; public uint Flags, Time; public nuint ExtraInfo; }
    [DllImport("user32.dll")] private static extern bool GetCursorPos(out ScreenPoint point);
    [DllImport("user32.dll")] private static extern bool SetCursorPos(int x, int y);
    [DllImport("user32.dll")] private static extern bool ClientToScreen(nint window, ref ScreenPoint point);
    [DllImport("user32.dll")] private static extern bool SetForegroundWindow(nint window);
    [DllImport("user32.dll")] private static extern nint GetForegroundWindow();
    [DllImport("user32.dll")] private static extern uint GetWindowThreadProcessId(nint window, out uint processId);
    [DllImport("user32.dll", SetLastError = true)] private static extern uint SendInput(uint count, Input[] values, int size);
}

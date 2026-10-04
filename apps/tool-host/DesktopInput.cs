using static KYNXA.ToolHost.DesktopNativeMethods;

namespace KYNXA.ToolHost;

internal static class DesktopInput
{
    private static readonly Dictionary<string, ushort[]> KeyCombinations = new(StringComparer.Ordinal)
    {
        ["ENTER"] = [0x0d], ["TAB"] = [0x09], ["ESC"] = [0x1b], ["BACKSPACE"] = [0x08], ["DELETE"] = [0x2e],
        ["UP"] = [0x26], ["DOWN"] = [0x28], ["LEFT"] = [0x25], ["RIGHT"] = [0x27], ["HOME"] = [0x24], ["END"] = [0x23],
        ["PAGEUP"] = [0x21], ["PAGEDOWN"] = [0x22], ["CTRL+A"] = [0x11, 0x41], ["CTRL+C"] = [0x11, 0x43],
        ["CTRL+V"] = [0x11, 0x56], ["CTRL+S"] = [0x11, 0x53], ["CTRL+F"] = [0x11, 0x46], ["CTRL+L"] = [0x11, 0x4c],
        ["CTRL+Z"] = [0x11, 0x5a], ["CTRL+Y"] = [0x11, 0x59], ["SHIFT+TAB"] = [0x10, 0x09], ["CTRL+ENTER"] = [0x11, 0x0d],
        ["CTRL+PLUS"] = [0x11, 0x6b], ["CTRL+MINUS"] = [0x11, 0x6d], ["CTRL+0"] = [0x11, 0x30]
    };

    internal static string[] SupportedKeys => KeyCombinations.Keys.ToArray();
    internal static Dictionary<string, object?> Apply(DesktopTarget target, DesktopRequest request, CancellationToken cancellation)
    {
        var sequence = new DesktopInputSequence();
        void Send(params Input[] events)
        {
            cancellation.ThrowIfCancellationRequested(); target.Check(foreground: true);
            sequence.Send(events);
        }
        static void EnsureIdle()
        {
            foreach (int key in new[] { 0x10, 0x11, 0x12, 0x5b, 0x5c, 0x01, 0x02, 0x04 })
                if ((GetAsyncKeyState(key) & 0x8000) != 0)
                    throw new DesktopException("DESKTOP_INPUT_BUSY", "A physical modifier or mouse button is held; input was not changed.");
        }
        EnsureIdle();
        try
        {
            if (request.Action == "type")
            {
                string text = request.Text ?? throw new DesktopException("DESKTOP_INVALID_REQUEST", "Text is required.");
                if (text.Length is 0 or > 4000 || text.Contains('\0')) throw new DesktopException("DESKTOP_INVALID_REQUEST", "Text is empty, too long or contains a NUL.");
                for (int index = 0; index < text.Length;)
                {
                    int end = Math.Min(text.Length, index + 128);
                    if (end < text.Length && char.IsHighSurrogate(text[end - 1])) end--;
                    var events = new List<Input>();
                    for (; index < end; index++) { events.Add(Unicode(text[index], false)); events.Add(Unicode(text[index], true)); }
                    EnsureIdle(); Send(events.ToArray());
                }
            }
            else if (request.Action == "key")
            {
                if (request.Key is null || !KeyCombinations.TryGetValue(request.Key, out ushort[]? keys))
                    throw new DesktopException("DESKTOP_INVALID_KEY", "The requested key combination is not supported.");
                if (keys.Any(key => (GetAsyncKeyState(key) & 0x8000) != 0))
                    throw new DesktopException("DESKTOP_INPUT_BUSY", "A requested key is physically held; input was not changed.");
                Send(keys.Select(key => Keyboard(key, false)).Concat(keys.Reverse().Select(key => Keyboard(key, true))).ToArray());
            }
            else
            {
                Point point = target.ClientPoint(request.X, request.Y);
                Input move = Move(point);
                switch (request.Action)
                {
                    case "move": Send(move); break;
                    case "click":
                        (uint down, uint up) = request.Button switch
                        {
                            "left" => (MouseLeftDown, MouseLeftUp), "right" => (MouseRightDown, MouseRightUp),
                            "middle" => (MouseMiddleDown, MouseMiddleUp),
                            _ => throw new DesktopException("DESKTOP_INVALID_REQUEST", "Only left, right and middle clicks are supported.")
                        };
                        Send(move, Mouse(down), Mouse(up));
                        break;
                    case "scroll":
                        if (request.Delta == 0 || request.Delta is < -1200 or > 1200 || request.Direction is not "vertical" and not "horizontal")
                            throw new DesktopException("DESKTOP_INVALID_REQUEST", "Scroll delta must be nonzero and bounded, with a valid direction.");
                        Send(move, Mouse(request.Direction == "vertical" ? MouseWheel : MouseHWheel, unchecked((uint)request.Delta)));
                        break;
                    case "drag":
                        target.ClientPoint(request.EndX, request.EndY);
                        Send(move, Mouse(MouseLeftDown));
                        for (int step = 1; step <= 10; step++)
                        {
                            if (cancellation.WaitHandle.WaitOne(30)) cancellation.ThrowIfCancellationRequested();
                            int x = request.X!.Value + (request.EndX!.Value - request.X.Value) * step / 10;
                            int y = request.Y!.Value + (request.EndY!.Value - request.Y.Value) * step / 10;
                            Send(Move(target.ClientPoint(x, y)));
                        }
                        Send(Mouse(MouseLeftUp));
                        break;
                    default: throw new DesktopException("DESKTOP_INVALID_REQUEST", "Unsupported input action.");
                }
            }
            target.Check(foreground: true);
            return new() { ["windowId"] = target.Window.ToInt64().ToString(), ["processId"] = target.ProcessId,
                ["deliveredInputEvents"] = sequence.DeliveredInputEvents, ["inputSent"] = true, ["coordinates"] = "client-physical-pixels" };
        }
        catch (OperationCanceledException) { throw new DesktopException("DESKTOP_CANCELLED", "Desktop input was cancelled.", sequence.DeliveredInputEvents); }
        catch (DesktopException error) when (error.DeliveredInputEvents < sequence.DeliveredInputEvents)
        { throw new DesktopException(error.Code, error.Message, sequence.DeliveredInputEvents); }
        finally { sequence.ReleaseOwnedPresses(); }
    }

    private static Input Mouse(uint flags, uint data = 0) => new() { Type = 0, Data = new InputUnion { Mouse = new MouseInput { Flags = flags, Data = data } } };
    private static Input Move(Point point)
    {
        int left = GetSystemMetrics(76), top = GetSystemMetrics(77), width = GetSystemMetrics(78), height = GetSystemMetrics(79);
        if (width < 2 || height < 2 || point.X < left || point.X >= left + width || point.Y < top || point.Y >= top + height)
            throw new DesktopException("DESKTOP_INVALID_COORDINATES", "The target point is outside the active virtual display.");
        return new() { Type = 0, Data = new InputUnion { Mouse = new MouseInput
        { X = (int)((long)(point.X - left) * 65535 / (width - 1)), Y = (int)((long)(point.Y - top) * 65535 / (height - 1)),
            Flags = MouseMove | MouseAbsolute | MouseVirtualDesk | MouseMoveNoCoalesce } } };
    }
    private static Input Keyboard(ushort key, bool up) => new() { Type = 1,
        Data = new InputUnion { Keyboard = new KeyboardInput { VirtualKey = key, Flags = up ? KeyUp : 0 } } };
    private static Input Unicode(char character, bool up) => new() { Type = 1,
        Data = new InputUnion { Keyboard = new KeyboardInput { Scan = character, Flags = KeyUnicode | (up ? KeyUp : 0) } } };
}

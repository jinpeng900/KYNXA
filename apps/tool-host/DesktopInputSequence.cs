using System.Runtime.InteropServices;
using static KYNXA.ToolHost.DesktopNativeMethods;

namespace KYNXA.ToolHost;

// Track the prefix actually accepted by SendInput, including partial batches.
// Cleanup releases only presses delivered by this operation, never unrelated physical keys.
internal sealed class DesktopInputSequence
{
    private readonly Func<Input[], uint> _send;
    private readonly Dictionary<(ushort VirtualKey, ushort Scan, bool Unicode), Input> _pressedKeys = [];
    private uint _pressedMouse;
    internal int DeliveredInputEvents { get; private set; }

    internal DesktopInputSequence(Func<Input[], uint>? send = null)
        => _send = send ?? (events => SendInput((uint)events.Length, events, Marshal.SizeOf<Input>()));

    internal void Send(Input[] events)
    {
        uint count = Deliver(events);
        if (count != events.Length)
            throw new DesktopException("DESKTOP_INPUT_BLOCKED", "SendInput did not deliver the complete input sequence; UIPI may block it.", DeliveredInputEvents);
    }

    private uint Deliver(Input[] events)
    {
        uint count = _send(events);
        if (count > events.Length) throw new DesktopException("DESKTOP_INPUT_BLOCKED", "The input sender returned an invalid event count.", DeliveredInputEvents);
        DeliveredInputEvents += (int)count;
        foreach (Input input in events.Take((int)count))
        {
            if (input.Type == 1)
            {
                KeyboardInput keyboard = input.Data.Keyboard;
                var key = (keyboard.VirtualKey, keyboard.Scan, (keyboard.Flags & KeyUnicode) != 0);
                if ((keyboard.Flags & KeyUp) != 0) _pressedKeys.Remove(key);
                else _pressedKeys.TryAdd(key, input);
            }
            else if (input.Type == 0)
            {
                uint flags = input.Data.Mouse.Flags;
                foreach ((uint down, uint up) in new[] { (MouseLeftDown, MouseLeftUp), (MouseRightDown, MouseRightUp), (MouseMiddleDown, MouseMiddleUp) })
                {
                    if ((flags & down) != 0) _pressedMouse |= down;
                    if ((flags & up) != 0) _pressedMouse &= ~down;
                }
            }
        }
        return count;
    }

    internal void ReleaseOwnedPresses()
    {
        // The target may have lost focus or cancellation may be set: key-up cleanup must still run.
        // Retry only the remaining presses, at most three bounded batches; never add a new down event.
        for (int attempt = 0; attempt < 3 && (_pressedKeys.Count > 0 || _pressedMouse != 0); attempt++)
        {
            var releases = new List<Input>();
            foreach (Input pressed in _pressedKeys.Values.Reverse())
            {
                Input released = pressed; released.Data.Keyboard.Flags |= KeyUp; releases.Add(released);
            }
            foreach ((uint down, uint up) in new[] { (MouseLeftDown, MouseLeftUp), (MouseRightDown, MouseRightUp), (MouseMiddleDown, MouseMiddleUp) })
                if ((_pressedMouse & down) != 0)
                    releases.Add(new Input { Type = 0, Data = new InputUnion { Mouse = new MouseInput { Flags = up } } });
            Deliver(releases.ToArray());
        }
        if (_pressedKeys.Count > 0 || _pressedMouse != 0)
            throw new DesktopException("DESKTOP_INPUT_RELEASE_FAILED", "Some input sent by this operation could not be released; the desktop input state is uncertain.", DeliveredInputEvents);
    }
}

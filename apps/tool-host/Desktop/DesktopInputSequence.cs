using System.Runtime.InteropServices;
using static KYNXA.ToolHost.DesktopNativeMethods;

namespace KYNXA.ToolHost;

// Track the prefix actually accepted by SendInput, including partial batches.
// Cleanup releases only presses delivered by this operation, never unrelated physical keys.
// 跟踪 SendInput 实际接收的事件前缀，包括部分批次；清理仅释放本次操作成功按下的键，不影响无关物理按键。
internal sealed class DesktopInputSequence
{
    private readonly Func<Input[], uint> _sendInputEvents;
    private readonly Dictionary<(ushort VirtualKey, ushort Scan, bool Unicode), Input> _pressedKeys = [];
    private uint _pressedMouseButtons;
    internal int DeliveredInputEvents { get; private set; }

    internal DesktopInputSequence(Func<Input[], uint>? sendInputEvents = null)
        => _sendInputEvents = sendInputEvents ?? (events => SendInput((uint)events.Length, events, Marshal.SizeOf<Input>()));

    internal void Send(Input[] events)
    {
        uint deliveredEventCount = Deliver(events);
        if (deliveredEventCount != events.Length)
            throw new DesktopException("DESKTOP_INPUT_BLOCKED", "SendInput did not deliver the complete input sequence; UIPI may block it.", DeliveredInputEvents);
    }

    private uint Deliver(Input[] events)
    {
        uint deliveredEventCount = _sendInputEvents(events);
        if (deliveredEventCount > events.Length) throw new DesktopException("DESKTOP_INPUT_BLOCKED", "The input sender returned an invalid event count.", DeliveredInputEvents);
        DeliveredInputEvents += (int)deliveredEventCount;
        foreach (Input input in events.Take((int)deliveredEventCount))
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
                foreach ((uint buttonDownFlag, uint buttonUpFlag) in new[] { (MouseLeftDown, MouseLeftUp), (MouseRightDown, MouseRightUp), (MouseMiddleDown, MouseMiddleUp) })
                {
                    if ((flags & buttonDownFlag) != 0) _pressedMouseButtons |= buttonDownFlag;
                    if ((flags & buttonUpFlag) != 0) _pressedMouseButtons &= ~buttonDownFlag;
                }
            }
        }
        return deliveredEventCount;
    }

    internal void ReleaseOwnedPresses()
    {
        // The target may have lost focus or cancellation may be set: key-up cleanup must still run.
        // Retry only the remaining presses, at most three bounded batches; never add a new down event.
        // 即使目标失焦或已取消，也必须执行松键清理；仅重试剩余按键，最多三批，不增加新的按下事件。
        for (int attempt = 0; attempt < 3 && (_pressedKeys.Count > 0 || _pressedMouseButtons != 0); attempt++)
        {
            var releases = new List<Input>();
            foreach (Input pressed in _pressedKeys.Values.Reverse())
            {
                Input released = pressed; released.Data.Keyboard.Flags |= KeyUp; releases.Add(released);
            }
            foreach ((uint buttonDownFlag, uint buttonUpFlag) in new[] { (MouseLeftDown, MouseLeftUp), (MouseRightDown, MouseRightUp), (MouseMiddleDown, MouseMiddleUp) })
                if ((_pressedMouseButtons & buttonDownFlag) != 0)
                    releases.Add(new Input { Type = 0, Data = new InputUnion { Mouse = new MouseInput { Flags = buttonUpFlag } } });
            Deliver(releases.ToArray());
        }
        if (_pressedKeys.Count > 0 || _pressedMouseButtons != 0)
            throw new DesktopException("DESKTOP_INPUT_RELEASE_FAILED", "Some input sent by this operation could not be released; the desktop input state is uncertain.", DeliveredInputEvents);
    }
}

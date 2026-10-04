using KYNXA.ToolHost;
using static KYNXA.ToolHost.DesktopNativeMethods;

// No desktop calls or windows: inject deterministic SendInput prefixes into the real cleanup module.
int checks = 0;
Input Mouse(uint flags) => new() { Type = 0, Data = new InputUnion { Mouse = new MouseInput { Flags = flags } } };
Input Key(ushort key, uint flags = 0, ushort scan = 0) => new() { Type = 1,
    Data = new InputUnion { Keyboard = new KeyboardInput { VirtualKey = key, Scan = scan, Flags = flags } } };
void Check(bool condition, string label) { if (!condition) throw new Exception(label); checks++; }
void Partial(uint prefix, Input[] action, Action<Input[][]> verify, uint? cleanupPrefix = null)
{
    var calls = new List<Input[]>();
    var sequence = new DesktopInputSequence(events =>
    {
        calls.Add(events.ToArray());
        return calls.Count == 1 ? prefix : calls.Count == 2 && cleanupPrefix.HasValue ? cleanupPrefix.Value : (uint)events.Length;
    });
    try { sequence.Send(action); throw new Exception("Partial input was falsely completed."); }
    catch (DesktopException error) { Check(error.Code == "DESKTOP_INPUT_BLOCKED" && error.DeliveredInputEvents == prefix, "Failed prefix receipt remains partial"); }
    finally { sequence.ReleaseOwnedPresses(); }
    verify(calls.ToArray());
}
Input[] click = [Mouse(MouseMove), Mouse(MouseLeftDown), Mouse(MouseLeftUp)];
Partial(0, click, calls => Check(calls.Length == 1, "Zero deliveries cause no releases"));
Partial(1, click, calls => Check(calls.Length == 1, "Move-only prefix does not release a physical button"));
Partial(2, click, calls => Check(calls.Length == 2 && calls[1].Length == 1 && calls[1][0].Data.Mouse.Flags == MouseLeftUp, "Partial click releases the delivered left down"));
Input[] chord = [Key(0x11), Key(0x41), Key(0x41, KeyUp), Key(0x11, KeyUp)];
Partial(1, chord, calls => Check(calls[1].Length == 1 && calls[1][0].Data.Keyboard.VirtualKey == 0x11
    && calls[1][0].Data.Keyboard.Flags == KeyUp, "Only delivered CTRL is released"));
Partial(2, chord, calls => Check(calls[1].Length == 2 && calls[1][0].Data.Keyboard.VirtualKey == 0x41
    && calls[1][1].Data.Keyboard.VirtualKey == 0x11 && calls[1].All(value => value.Data.Keyboard.Flags == KeyUp), "Partial chord releases both owned keys in reverse order"));
Partial(3, chord, calls => Check(calls[1].Length == 1 && calls[1][0].Data.Keyboard.VirtualKey == 0x11, "Already released A is not released again"));
Partial(1, [Key(0, KeyUnicode, '中'), Key(0, KeyUnicode | KeyUp, '中')], calls => Check(calls[1][0].Data.Keyboard.Scan == '中'
    && calls[1][0].Data.Keyboard.Flags == (KeyUnicode | KeyUp), "Unicode prefix uses matching scan release"));
Partial(2, chord, calls => Check(calls.Length == 3 && calls[2].Length == 1 && calls[2][0].Data.Keyboard.VirtualKey == 0x11,
    "Cleanup retry releases only remaining press"), cleanupPrefix: 1);
foreach ((uint down, uint up) in new[] { (MouseRightDown, MouseRightUp), (MouseMiddleDown, MouseMiddleUp) })
    Partial(1, [Mouse(down), Mouse(up)], calls => Check(calls[1].Single().Data.Mouse.Flags == up, "Right/middle own press released"));
var completedCalls = new List<Input[]>();
var completed = new DesktopInputSequence(events => { completedCalls.Add(events); return (uint)events.Length; });
completed.Send(chord); completed.ReleaseOwnedPresses();
Check(completedCalls.Count == 1 && completed.DeliveredInputEvents == 4, "Complete chord does not add unrelated cleanup");
var dragCalls = new List<Input[]>();
var drag = new DesktopInputSequence(events => { dragCalls.Add(events); return (uint)events.Length; });
try { drag.Send([Mouse(MouseMove), Mouse(MouseLeftDown)]); throw new OperationCanceledException(); }
catch (OperationCanceledException) { }
finally { drag.ReleaseOwnedPresses(); }
Check(dragCalls.Count == 2 && dragCalls[1].Single().Data.Mouse.Flags == MouseLeftUp, "Cancellation releases successful drag down");
var blockedCleanup = new List<Input[]>();
var blocked = new DesktopInputSequence(events => { blockedCleanup.Add(events); return blockedCleanup.Count == 1 ? (uint)events.Length : 0; });
blocked.Send([Key(0x11)]);
try { blocked.ReleaseOwnedPresses(); throw new Exception("Failed cleanup was falsely completed."); }
catch (DesktopException error) { Check(error.Code == "DESKTOP_INPUT_RELEASE_FAILED" && error.DeliveredInputEvents == 1, "Failed cleanup reports uncertain partial state"); }
Check(blockedCleanup.Count == 4 && blockedCleanup.Skip(1).All(events => events.Length == 1 && events[0].Data.Keyboard.Flags == KeyUp), "Blocked cleanup retries bounded key-up only");
Console.WriteLine($"PASS native input prefix cleanup: {checks} checks; no desktop interaction.");

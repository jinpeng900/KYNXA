using System.Diagnostics;
using System.IO;
using System.Text.Json;
using System.Windows;
using System.Windows.Automation.Peers;
using System.Windows.Controls;
using System.Windows.Input;
using System.Windows.Interop;
using System.Windows.Media.Imaging;
using System.Runtime.InteropServices;

internal static partial class Program
{
    private static async Task CheckCapabilityExtensionsAsync(Window window, bool physicalInput)
    {
        string id = new WindowInteropHelper(window).Handle.ToInt64().ToString(); int pid = Environment.ProcessId;
        JsonElement capabilities = await RequestAsync(new { operation = "desktop_capabilities" });
        Check(capabilities.GetProperty("operations").EnumerateArray().Any(x => x.GetString() == "window"), "Window state operation available");
        Check(new[] { "CTRL+PLUS", "CTRL+MINUS", "CTRL+0" }.All(key => capabilities.GetProperty("keys").EnumerateArray().Any(x => x.GetString() == key)), "Zoom key capability agreement");
        JsonElement list = await RequestAsync(new { operation = "desktop", action = "windows", processId = pid });
        Check(list.GetProperty("windows").EnumerateArray().First(x => x.GetProperty("windowId").GetString() == id).GetProperty("isResponding").GetBoolean(), "Synthetic window response status");
        JsonElement read = await RequestAsync(new { operation = "desktop", action = "read", windowId = id, processId = pid });
        JsonElement password = read.GetProperty("elements").EnumerateArray().Single(x => x.GetProperty("isPassword").GetBoolean());
        Check(password.GetProperty("automationId").GetString() == "fixture-password" && password.GetProperty("elementId").GetString()!.Length > 0,
            "Password retains locator metadata");
        Check(password.GetProperty("name").GetString() == "" && password.GetProperty("value").GetString() == "" && !read.GetRawText().Contains("SYNTHETIC_PASSWORD"),
            "Password name, value and text are not exported");
        if (physicalInput)
        {
            Password.Password = ""; // Fixture initialization; Ctrl+A is not supported by every password control.
            window.Activate(); await Task.Delay(150);
            Check(GetForegroundWindow() == new WindowInteropHelper(window).Handle, "Owned fixture explicitly obtains foreground before physical input");
            await RequestAsync(new { operation = "desktop", action = "activate", windowId = id, processId = pid });
            JsonElement clickedPassword = await RequestAsync(new { operation = "desktop", action = "click", windowId = id, processId = pid,
                x = (int)(password.GetProperty("x").GetDouble() + 10), y = (int)(password.GetProperty("y").GetDouble() + 10) });
            Check(clickedPassword.TryGetProperty("completed", out var clickCompleted) && clickCompleted.GetBoolean(), "Password click has an actual completion receipt: " +
                (clickedPassword.TryGetProperty("error", out var clickError) ? clickError.GetProperty("code").GetString() : "missing"));
            await Task.Delay(80); Check(Password.IsKeyboardFocused, "Password click focuses the selected synthetic control");
            JsonElement typedPassword = await RequestAsync(new { operation = "desktop", action = "type", windowId = id, processId = pid, text = "OWNED_NEW_PASSWORD_中文" });
            Check(typedPassword.TryGetProperty("completed", out var typeCompleted) && typeCompleted.GetBoolean(), "Password input has an actual completion receipt");
            await Task.Delay(80); Check(Password.Password == "OWNED_NEW_PASSWORD_中文", "Authorized physical input writes the owned password control");
        }
        JsonElement passwordOnly = await RequestAsync(new { operation = "desktop", action = "read", windowId = id, processId = pid,
            elementId = password.GetProperty("elementId").GetString(), timeoutMs = 1500 });
        Check(passwordOnly.GetProperty("text").GetString() == "" && passwordOnly.GetProperty("elements").GetArrayLength() == 1 &&
            !passwordOnly.GetRawText().Contains("OWNED_NEW_PASSWORD"), "Password element-specific read contains only location metadata");
        JsonElement editable = read.GetProperty("elements").EnumerateArray().First(x => x.GetProperty("name").GetString() == "Fixture editable text");
        JsonElement selected = await RequestAsync(new { operation = "desktop", action = "read", windowId = id, processId = pid,
            elementId = editable.GetProperty("elementId").GetString(), timeoutMs = 1500 });
        Check(selected.GetProperty("text").GetString()!.Contains("initial fixture") && !selected.GetProperty("text").GetString()!.Contains("Fixture button"),
            "Element read is confined to the selected owned subtree");
        var region = new { x = (int)editable.GetProperty("x").GetDouble(), y = (int)editable.GetProperty("y").GetDouble(),
            width = (int)editable.GetProperty("width").GetDouble(), height = (int)editable.GetProperty("height").GetDouble() };
        JsonElement scoped = await RequestAsync(new { operation = "desktop", action = "read", windowId = id, processId = pid, region });
        Check(scoped.GetProperty("text").GetString()!.Contains("initial fixture") && !scoped.GetProperty("text").GetString()!.Contains("Fixture button"), "Region excludes unrelated controls");
        CheckError(await RequestAsync(new { operation = "desktop", action = "read", windowId = id, processId = pid, elementId = "42,2147483647" }), "DESKTOP_ELEMENT_NOT_FOUND");
        JsonElement whole = await RequestAsync(new { operation = "desktop", action = "screenshot", windowId = id, processId = pid });
        JsonElement cropped = await RequestAsync(new { operation = "desktop", action = "screenshot", windowId = id, processId = pid,
            crop = new { x = 30, y = 60, width = 80, height = 40 } });
        using var fullStream = new MemoryStream(Convert.FromBase64String(whole.GetProperty("data").GetString()!));
        using var cropStream = new MemoryStream(Convert.FromBase64String(cropped.GetProperty("data").GetString()!));
        BitmapFrame fullFrame = BitmapDecoder.Create(fullStream, BitmapCreateOptions.PreservePixelFormat, BitmapCacheOption.OnLoad).Frames[0];
        BitmapFrame cropFrame = BitmapDecoder.Create(cropStream, BitmapCreateOptions.PreservePixelFormat, BitmapCacheOption.OnLoad).Frames[0];
        Check(cropFrame.PixelWidth == 80 && cropFrame.PixelHeight == 40 && cropped.GetProperty("crop").GetProperty("x").GetInt32() == 30, "Screenshot crops original physical dimensions");
        int stride = (80 * fullFrame.Format.BitsPerPixel + 7) / 8;
        byte[] originalPixels = new byte[stride * 40], croppedPixels = new byte[stride * 40];
        fullFrame.CopyPixels(new Int32Rect(30, 60, 80, 40), originalPixels, stride, 0);
        cropFrame.CopyPixels(croppedPixels, stride, 0);
        Check(originalPixels.SequenceEqual(croppedPixels), "Crop pixels are unchanged without scaling");
        await File.WriteAllBytesAsync(Path.Combine(ArtifactFolder, "synthetic-crop.png"), Convert.FromBase64String(cropped.GetProperty("data").GetString()!));
        CheckError(await RequestAsync(new { operation = "desktop", action = "screenshot", windowId = id, processId = pid,
            crop = new { x = 30000, y = 0, width = 20, height = 20 } }), "DESKTOP_INVALID_COORDINATES");
        if (physicalInput)
        {
            var zoomKeys = new List<Key>();
            window.PreviewKeyDown += (_, e) => { if ((Keyboard.Modifiers & ModifierKeys.Control) != 0) zoomKeys.Add(e.Key); };
            foreach (string key in new[] { "CTRL+PLUS", "CTRL+MINUS", "CTRL+0" })
                await RequestAsync(new { operation = "desktop", action = "key", windowId = id, processId = pid, key });
            await Task.Delay(80);
            Check(zoomKeys.Contains(Key.Add) && zoomKeys.Contains(Key.Subtract) && zoomKeys.Contains(Key.D0), "Actual zoom key events delivered");
        }
        var foreground = new Window { Title = "Owned background guard", Width = 240, Height = 160, Left = 760, Top = 80, ShowActivated = false };
        foreground.Show(); await Task.Delay(100);
        nint foregroundId = GetForegroundWindow();
        try
        {
            JsonElement resized = await RequestAsync(new { operation = "desktop", action = "window", windowId = id, processId = pid, mode = "resize", width = 680, height = 580 });
            Check(resized.GetProperty("clientWidth").GetInt32() == 680 && resized.GetProperty("clientHeight").GetInt32() == 580, "Window resize uses actual client dimensions");
            Check(resized.GetProperty("foregroundPreserved").GetBoolean() && GetForegroundWindow() == foregroundId, "Background resize preserves foreground");
            foreach (string mode in new[] { "maximize", "minimize", "restore" })
            {
                JsonElement state = await RequestAsync(new { operation = "desktop", action = "window", windowId = id, processId = pid, mode });
                Check(state.TryGetProperty("completed", out var applied) && applied.GetBoolean() && GetForegroundWindow() == foregroundId, "Background window " + mode + " preserves foreground: " +
                    (state.TryGetProperty("error", out var windowError) ? windowError.GetProperty("code").GetString() : "foreground changed"));
                Check(mode switch { "maximize" => state.GetProperty("isMaximized").GetBoolean(), "minimize" => state.GetProperty("isMinimized").GetBoolean(),
                    _ => !state.GetProperty("isMinimized").GetBoolean() && !state.GetProperty("isMaximized").GetBoolean() }, "Confirmed actual window " + mode);
            }
            await ChildCheckAsync("--background-fixture", async (child, ready) =>
            {
                Check(GetForegroundWindow() == foregroundId, "Background GUI fixture stays behind the owned foreground window");
                JsonElement windows = await RequestAsync(new { operation = "desktop", action = "windows", processId = child.Id });
                Check(windows.GetProperty("windows").EnumerateArray().All(x => !x.GetProperty("isForeground").GetBoolean()), "Background launch actual window state is not foreground");
            });
            await ChildCheckAsync("--hung-fixture", async (child, ready) =>
            {
                await Task.Delay(250);
                var timer = Stopwatch.StartNew();
                CheckError(await RequestAsync(new { operation = "desktop", action = "read", windowId = ready.GetProperty("windowId").GetString(), processId = child.Id, timeoutMs = 1000 }), "DESKTOP_NOT_RESPONDING");
                Check(timer.ElapsedMilliseconds < 2000, "Unresponsive owned window rejected before entering UIA");
            });
            await ChildCheckAsync("--slow-provider-fixture", async (child, ready) =>
            {
                var timer = Stopwatch.StartNew();
                CheckError(await RequestAsync(new { operation = "desktop", action = "read", windowId = ready.GetProperty("windowId").GetString(), processId = child.Id, timeoutMs = 1000 }), "DESKTOP_READ_TIMEOUT");
                Check(timer.ElapsedMilliseconds < 2500, "Stalled UIA provider is bounded by the request deadline");
            });
        }
        finally { foreground.Close(); }
    }

    private static async Task ChildCheckAsync(string mode, Func<Process, JsonElement, Task> check)
    {
        string directory = Path.Combine(ArtifactFolder, mode.TrimStart('-'));
        JsonElement launch = await RequestAsync(new { operation = "desktop", action = "launch", appPath = Environment.ProcessPath,
            args = new[] { mode, directory }, background = true });
        Check(launch.GetProperty("backgroundRequested").GetBoolean() && launch.GetProperty("backgroundMode").GetString() == "best-effort-no-activate", "Honest background launch receipt");
        using var child = Process.GetProcessById(launch.GetProperty("processId").GetInt32());
        try
        {
            string filename = Path.Combine(directory, "ready.json");
            for (int index = 0; index < 50 && !File.Exists(filename); index++) await Task.Delay(100);
            Check(File.Exists(filename), "Only the independently launched synthetic child is ready");
            JsonElement ready = JsonDocument.Parse(await File.ReadAllTextAsync(filename)).RootElement.Clone();
            Check(ready.GetProperty("processId").GetInt32() == child.Id, "Child fixture identity matches launch receipt");
            await check(child, ready);
        }
        finally { if (!child.HasExited) child.Kill(entireProcessTree: true); await child.WaitForExitAsync(); }
    }

    private sealed class SlowNameBlock : TextBlock
    {
        protected override AutomationPeer OnCreateAutomationPeer() => new SlowNamePeer(this);
    }
    private sealed class SlowNamePeer(SlowNameBlock owner) : TextBlockAutomationPeer(owner)
    {
        protected override string GetNameCore() { Thread.Sleep(7000); return "Owned slow UIA name"; }
    }
    [DllImport("user32.dll")] private static extern nint GetForegroundWindow();
}

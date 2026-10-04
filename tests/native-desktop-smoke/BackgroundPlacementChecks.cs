using System.Diagnostics;
using System.IO;
using System.Text.Json;
using System.Windows;
using System.Windows.Interop;
using System.Windows.Media;
using System.Windows.Media.Imaging;

internal static partial class Program
{
    private static async Task CheckBackgroundPlacementAsync(Window window)
    {
        window.Activate();
        await Task.Delay(100);
        nint foregroundId = new WindowInteropHelper(window).Handle;
        Check(GetForegroundWindow() == foregroundId, "Owned fixture obtains ordinary foreground before background checks");
        string folder = Path.Combine(ArtifactFolder, "delayed-activation");
        int childPid = 0;
        try
        {
            // Background is deliberately omitted to verify the native default. The fixture
            // deliberately activates after loading to exercise delayed focus/placement repair.
            JsonElement launched = await RequestAsync(new { operation = "desktop", action = "launch",
                appPath = Environment.ProcessPath, args = new[] { "--delayed-activation-fixture", folder } });
            childPid = launched.GetProperty("processId").GetInt32();
            Check(launched.GetProperty("backgroundRequested").GetBoolean(), "GUI launch defaults to background placement");
            Check(launched.GetProperty("backgroundWindowObserved").GetBoolean(), "Startup observation sees the actual delayed child window");
            Check(launched.GetProperty("backgroundPlacementConfirmed").GetBoolean(), "Observed child is behind the original foreground window");
            Check(launched.GetProperty("foregroundPreserved").GetBoolean() && GetForegroundWindow() == foregroundId,
                "Delayed GUI activation restores ordinary fixture foreground");
            string readyPath = Path.Combine(folder, "ready.json");
            for (int attempt = 0; attempt < 40 && !File.Exists(readyPath); attempt++) await Task.Delay(50);
            Check(File.Exists(readyPath), "Launched fixture reports only its own target identity");
            JsonElement ready = JsonDocument.Parse(await File.ReadAllTextAsync(readyPath)).RootElement;
            string childWindow = ready.GetProperty("windowId").GetString()!;
            Check(ready.GetProperty("processId").GetInt32() == childPid, "Background target PID belongs to the launched child");
            foreach (string mode in new[] { "resize", "maximize", "minimize", "restore" })
            {
                JsonElement state = await RequestAsync(new { operation = "desktop", action = "window", windowId = childWindow,
                    processId = childPid, mode, width = 500, height = 490 });
                Check(state.GetProperty("completed").GetBoolean(), "Background target applies " + mode);
                Check(GetForegroundWindow() == foregroundId && state.GetProperty("foregroundPreserved").GetBoolean(),
                    "Background " + mode + " keeps the selected fixture in front");
                if (mode != "minimize")
                    Check(state.GetProperty("backgroundPlacementConfirmed").GetBoolean(), "Background " + mode + " remains behind the fixture");
            }
            JsonElement image = await RequestAsync(new { operation = "desktop", action = "screenshot", windowId = childWindow, processId = childPid });
            Check(image.GetProperty("captureMethod").GetString() == "print-window-client", "Occluded target is captured independently of foreground screen");
            byte[] data = Convert.FromBase64String(image.GetProperty("data").GetString()!);
            using var stream = new MemoryStream(data);
            BitmapFrame frame = BitmapDecoder.Create(stream, BitmapCreateOptions.PreservePixelFormat, BitmapCacheOption.OnLoad).Frames[0];
            var pixels = new FormatConvertedBitmap(frame, PixelFormats.Bgra32, null, 0);
            var pixel = new byte[4];
            pixels.CopyPixels(new Int32Rect(3, 3, 1, 1), pixel, 4, 0);
            Check(pixel[0] == 224 && pixel[1] == 255 && pixel[2] == 255,
                "Screenshot retains the hidden-behind LightYellow target pixels rather than the white foreground fixture");
            Check(GetForegroundWindow() == foregroundId, "Target screenshot never activates the background application");
            await File.WriteAllBytesAsync(Path.Combine(ArtifactFolder, "background-target.png"), data);
            await CheckVisibleTerminalPlacementAsync(foregroundId);
        }
        finally
        {
            if (childPid > 0)
            {
                using Process child = Process.GetProcessById(childPid);
                if (!child.HasExited) child.Kill(entireProcessTree: true);
                await child.WaitForExitAsync();
            }
        }
    }

    private static async Task CheckVisibleTerminalPlacementAsync(nint foregroundId)
    {
        string cwd = Directory.CreateDirectory(Path.Combine(ArtifactFolder, "visible-console")).FullName;
        using Process process = Process.Start(new ProcessStartInfo(Host)
        {
            UseShellExecute = false, CreateNoWindow = true, RedirectStandardInput = true,
            RedirectStandardOutput = true, RedirectStandardError = true
        }) ?? throw new Exception("Cannot start owned visible-console helper.");
        try
        {
            await process.StandardInput.WriteLineAsync(JsonSerializer.Serialize(new
            {
                operation = "host_terminal_visible", shell = "cmd", script = "echo BACKGROUND_CONSOLE_FIXTURE",
                cwd, visible = true, keepOpenMs = 1500, timeoutMs = 10000
            }, JsonOptions));
            await process.StandardInput.FlushAsync();
            bool observed = false;
            JsonElement receipt = default;
            while (await process.StandardOutput.ReadLineAsync().WaitAsync(TimeSpan.FromSeconds(15)) is { } line)
            {
                JsonElement record = JsonDocument.Parse(line).RootElement.Clone();
                if (!record.TryGetProperty("event", out JsonElement eventName)) { receipt = record; break; }
                if (eventName.GetString() != "host_terminal_started") continue;
                observed = true;
                Check(record.GetProperty("backgroundRequested").GetBoolean() && record.GetProperty("windowObserved").GetBoolean(),
                    "A genuine visible console is started with background placement");
                Check(GetForegroundWindow() == foregroundId && record.GetProperty("foregroundPreserved").GetBoolean(),
                    "Visible console does not replace the selected fixture's foreground");
                Check(record.GetProperty("backgroundPlacementConfirmed").GetBoolean(), "The actual console is behind the selected fixture");
                JsonElement image = await RequestAsync(new
                {
                    operation = "desktop", action = "screenshot", windowId = record.GetProperty("windowId").GetString(),
                    processId = record.GetProperty("windowProcessId").GetInt32()
                });
                Check(image.TryGetProperty("data", out _) && image.GetProperty("width").GetInt32() > 0,
                    "Visible background console remains capturable without bringing it to the front");
                await File.WriteAllBytesAsync(Path.Combine(ArtifactFolder, "background-console.png"), Convert.FromBase64String(image.GetProperty("data").GetString()!));
                Check(GetForegroundWindow() == foregroundId, "Console screenshot preserves foreground");
            }
            await process.WaitForExitAsync().WaitAsync(TimeSpan.FromSeconds(5));
            Check(observed, "Visible terminal returns an actual start receipt");
            Check(receipt.GetProperty("completed").GetBoolean() && receipt.GetProperty("exitCode").GetInt32() == 0,
                "Background visible command completes normally");
            Check(receipt.GetProperty("consoleText").GetString()!.Contains("BACKGROUND_CONSOLE_FIXTURE"),
                "Background placement preserves console output receipts");
            Check(GetForegroundWindow() == foregroundId, "Console cleanup preserves the foreground fixture");
        }
        finally
        {
            if (!process.HasExited) process.Kill();
            await process.WaitForExitAsync();
        }
    }
}

using System.Diagnostics;
using System.IO;
using System.Runtime.InteropServices;
using System.Text.Json;
using System.Windows;
using System.Windows.Automation;
using System.Windows.Controls;
using System.Windows.Input;
using System.Windows.Interop;
using System.Windows.Media;
using System.Windows.Media.Imaging;

internal static partial class Program
{
    private static readonly JsonSerializerOptions JsonOptions = new(JsonSerializerDefaults.Web);
    private static int _checks;
    private static int _buttonClicks, _wheelEvents, _dragEvents, _moveEvents, _middleEvents;
    private static readonly TextBox Input = new() { Height = 38, Margin = new Thickness(12), Text = "initial fixture" };
    private static readonly Button Button = new() { Content = "Fixture button", Height = 36, Margin = new Thickness(12) };
    private static readonly PasswordBox Password = new() { Password = "SYNTHETIC_PASSWORD_MUST_NOT_BE_EXPORTED", Height = 26, Margin = new Thickness(12) };
    private static readonly Border DragBox = new() { Background = Brushes.SkyBlue, Height = 55, Margin = new Thickness(12),
        Child = new TextBlock { Text = "Synthetic drag target", Margin = new Thickness(8) } };
    private static readonly ScrollViewer Scroll = new() { Height = 80, Margin = new Thickness(12),
        Content = new TextBlock { Text = string.Join("\n", Enumerable.Range(1, 30).Select(i => "Synthetic scroll row " + i)) } };

    [STAThread]
    private static int Main(string[] args)
    {
        Console.OutputEncoding = System.Text.Encoding.UTF8;
        var application = new Application();
        var panel = new StackPanel();
        panel.Children.Add(new TextBlock { Text = "Synthetic KYNXA desktop fixture", Margin = new Thickness(12) });
        var address = new TextBox { Text = "https://example.invalid/native-fixture", IsReadOnly = true, Height = 26, Margin = new Thickness(12) };
        AutomationProperties.SetName(address, "Fixture address"); panel.Children.Add(address);
        AutomationProperties.SetName(Input, "Fixture editable text"); panel.Children.Add(Input);
        AutomationProperties.SetName(Password, "SYNTHETIC_PASSWORD_NAME_MUST_NOT_BE_EXPORTED");
        AutomationProperties.SetAutomationId(Password, "fixture-password");
        panel.Children.Add(Password); panel.Children.Add(Button); panel.Children.Add(DragBox); panel.Children.Add(Scroll);
        if (args is ["--slow-provider-fixture", _]) panel.Children.Add(new SlowNameBlock { Text = "Owned slow provider fixture" });
        Button.Click += (_, _) => _buttonClicks++;
        DragBox.PreviewMouseDown += (_, eventArgs) => { _dragEvents++; if (eventArgs.ChangedButton == MouseButton.Middle) _middleEvents++; };
        Scroll.PreviewMouseWheel += (_, _) => _wheelEvents++;
        var window = new Window { Title = "Synthetic KYNXA desktop fixture", Width = 540, Height = 540,
            Left = 80, Top = 80, Background = Brushes.White, Content = panel };
        if (args is ["--delayed-activation-fixture", _]) window.Background = Brushes.LightYellow;
        if (args is ["--background-fixture" or "--slow-provider-fixture" or "--hung-fixture", _]) window.ShowActivated = false;
        window.PreviewMouseMove += (_, _) => _moveEvents++;
        window.Loaded += async (_, _) =>
        {
            if (args.Length == 2 && args[0] is "--fixture" or "--background-fixture" or "--slow-provider-fixture" or "--hung-fixture" or "--delayed-activation-fixture")
            {
                Directory.CreateDirectory(args[1]);
                if (args[0] == "--delayed-activation-fixture") { await Task.Delay(450); window.Activate(); }
                await File.WriteAllTextAsync(Path.Combine(args[1], "ready.json"), JsonSerializer.Serialize(new
                { windowId = new WindowInteropHelper(window).Handle.ToInt64().ToString(), processId = Environment.ProcessId }, JsonOptions));
                if (args[0] == "--hung-fixture") _ = window.Dispatcher.InvokeAsync(() => Thread.Sleep(9000));
                return;
            }
            try
            {
                if (args is ["--capability-extensions-only" or "--capability-readonly-only"]) await CheckCapabilityExtensionsAsync(window, args[0] != "--capability-readonly-only");
                else if (args is ["--background-placement-only"]) await CheckBackgroundPlacementAsync(window);
                else if (args is ["--input-extensions-only"]) await CheckInputExtensionsAsync(window);
                else await CheckAsync(window);
                File.WriteAllText(Path.Combine(ArtifactFolder, "result.json"), JsonSerializer.Serialize(new { passed = true, checks = _checks }, JsonOptions));
                Console.WriteLine($"PASS native desktop: {_checks} checks; artifacts: {ArtifactFolder}");
                application.Shutdown(0);
            }
            catch (Exception error)
            {
                File.WriteAllText(Path.Combine(ArtifactFolder, "result.json"), JsonSerializer.Serialize(new
                { passed = false, checks = _checks, error = error.ToString() }, JsonOptions));
                Console.Error.WriteLine(error); application.Shutdown(1);
            }
        };
        return application.Run(window);
    }

    private static string ArtifactFolder { get; } = Directory.CreateDirectory(Path.Combine(Path.GetTempPath(), "kynxa-native-desktop-" + Guid.NewGuid().ToString("N"))).FullName;
    private static string Host => Environment.GetEnvironmentVariable("KYNXA_DESKTOP_SMOKE_TOOL_HOST")
        ?? Path.GetFullPath(Path.Combine(AppContext.BaseDirectory, "../../../../../apps/tool-host/bin/Debug/net10.0-windows/win-x64/KYNXA.ToolHost.exe"));

    private static async Task<JsonElement> RequestAsync(object request)
    {
        using var process = Process.Start(new ProcessStartInfo(Host) { UseShellExecute = false, CreateNoWindow = true,
            RedirectStandardInput = true, RedirectStandardOutput = true, RedirectStandardError = true }) ?? throw new Exception("Cannot start synthetic helper.");
        await process.StandardInput.WriteLineAsync(JsonSerializer.Serialize(request, JsonOptions));
        await process.StandardInput.FlushAsync();
        string? response = await process.StandardOutput.ReadLineAsync().WaitAsync(TimeSpan.FromSeconds(20));
        await process.WaitForExitAsync().WaitAsync(TimeSpan.FromSeconds(5));
        if (response is null) throw new Exception("Missing helper response: " + await process.StandardError.ReadToEndAsync());
        return JsonDocument.Parse(response).RootElement.Clone();
    }

    private static async Task CheckAsync(Window window)
    {
        string id = new WindowInteropHelper(window).Handle.ToInt64().ToString(); int pid = Environment.ProcessId;
        object Target(string action) => new { operation = "desktop", action, windowId = id, processId = pid };
        JsonElement capability = await RequestAsync(new { operation = "desktop_capabilities" });
        Check(capability.GetProperty("available").GetBoolean() && capability.GetProperty("interactiveWindows").GetBoolean(), "Interactive capabilities");
        Check(capability.GetProperty("boundary").GetString() == "host-desktop", "Honest desktop boundary");
        Check(capability.GetProperty("operations").GetArrayLength() == 13, "Thirteen native operations");
        JsonElement apps = await RequestAsync(new { operation = "desktop", action = "apps" });
        Check(apps.GetProperty("apps").GetArrayLength() <= 128 && apps.GetProperty("source").GetString() == "known-apps-and-registry-app-paths", "Bounded registry application discovery");
        Check(apps.GetProperty("apps").EnumerateArray().All(app => Path.IsPathFullyQualified(app.GetProperty("appPath").GetString()!)), "Discovered application paths are absolute");
        JsonElement sandbox = await RequestAsync(new { operation = "capabilities" });
        Check(sandbox.GetProperty("sandbox").GetString() == "appcontainer", "Sandbox capability unchanged");
        JsonElement listed = await RequestAsync(new { operation = "desktop", action = "windows", processId = pid });
        Check(listed.GetProperty("windows").EnumerateArray().Any(item => item.GetProperty("windowId").GetString() == id), "Own synthetic window discovery");
        Check(listed.GetProperty("windows").EnumerateArray().All(item => item.GetProperty("processId").GetInt32() == pid), "Discovery filter excludes other processes");
        JsonElement read = await RequestAsync(Target("read"));
        Check(read.GetProperty("source").GetString() == "uia-visible", "UIA source explicit");
        Check(read.GetProperty("text").GetString()!.Contains("Synthetic KYNXA desktop fixture"), "Visible synthetic text");
        Check(read.GetProperty("accessibleUrls").EnumerateArray().Any(item => item.GetString() == "https://example.invalid/native-fixture"), "Accessible fixture URL");
        Check(!read.GetRawText().Contains("SYNTHETIC_PASSWORD_MUST_NOT_BE_EXPORTED"), "Password is never read");
        JsonElement screenshot = await RequestAsync(Target("screenshot"));
        byte[] png = Convert.FromBase64String(screenshot.GetProperty("data").GetString()!);
        Check(png.Length <= 4 * 1024 * 1024 && png.AsSpan(0, 8).SequenceEqual(new byte[] { 137, 80, 78, 71, 13, 10, 26, 10 }), "Bounded genuine PNG");
        await File.WriteAllBytesAsync(Path.Combine(ArtifactFolder, "synthetic-window.png"), png);
        using (var stream = new MemoryStream(png))
        {
            BitmapFrame frame = BitmapDecoder.Create(stream, BitmapCreateOptions.PreservePixelFormat, BitmapCacheOption.OnLoad).Frames[0];
            Check(frame.PixelWidth == screenshot.GetProperty("width").GetInt32() && frame.PixelHeight == screenshot.GetProperty("height").GetInt32(), "Physical client dimensions");
        }
        CheckError(await RequestAsync(new { operation = "desktop", action = "read", windowId = id, processId = pid + 1 }), "DESKTOP_TARGET_CHANGED");
        CheckError(await RequestAsync(new { operation = "desktop", action = "click", windowId = id, processId = pid, x = -1, y = 1 }), "DESKTOP_INVALID_COORDINATES");
        CheckError(await RequestAsync(new { operation = "desktop", action = "key", windowId = id, processId = pid, key = "WIN+L" }), "DESKTOP_INVALID_KEY");
        CheckError(await RequestAsync(new { operation = "desktop", action = "type", windowId = id, processId = pid, text = new string('x', 4001) }), "DESKTOP_INVALID_REQUEST");
        CheckError(await RequestAsync(new { operation = "desktop", action = "launch", appPath = Path.Combine(Environment.GetFolderPath(Environment.SpecialFolder.System), "cmd.exe"), args = new[] { "/c", "echo fixture" } }), "DESKTOP_LAUNCH_BLOCKED");
        JsonElement activated = await RequestAsync(Target("activate")); Check(activated.GetProperty("activated").GetBoolean(), "Synthetic activation");
        async Task<Point> Coordinates(FrameworkElement element) => await window.Dispatcher.InvokeAsync(() =>
        {
            Point point = element.PointToScreen(new Point(Math.Min(16, element.ActualWidth / 2), element.ActualHeight / 2));
            NativePoint origin = new(); ClientToScreen(new WindowInteropHelper(window).Handle, ref origin);
            return new Point(point.X - origin.X, point.Y - origin.Y);
        });
        Point inputPoint = await Coordinates(Input);
        await RequestAsync(new { operation = "desktop", action = "click", windowId = id, processId = pid, x = (int)inputPoint.X, y = (int)inputPoint.Y });
        await RequestAsync(new { operation = "desktop", action = "key", windowId = id, processId = pid, key = "CTRL+A" });
        Check(!(await RequestAsync(new { operation = "desktop", action = "type", windowId = id, processId = pid, text = "中文 English fixture" })).TryGetProperty("error", out _), "Unicode input delivered");
        await Task.Delay(120);
        Check(Input.Text == "中文 English fixture", "Actual bilingual text result");
        Point button = await Coordinates(Button);
        await RequestAsync(new { operation = "desktop", action = "move", windowId = id, processId = pid, x = (int)button.X, y = (int)button.Y });
        await RequestAsync(new { operation = "desktop", action = "click", windowId = id, processId = pid, x = (int)button.X, y = (int)button.Y });
        await Task.Delay(100); Check(_moveEvents > 0 && _buttonClicks == 1, "Real mouse move and button click");
        Point scroll = await Coordinates(Scroll);
        await RequestAsync(new { operation = "desktop", action = "scroll", windowId = id, processId = pid, x = (int)scroll.X, y = (int)scroll.Y, delta = -120 });
        await Task.Delay(100); Check(_wheelEvents > 0 && Scroll.VerticalOffset > 0, "Actual scrolling");
        Point drag = await Coordinates(DragBox);
        await RequestAsync(new { operation = "desktop", action = "click", windowId = id, processId = pid, button = "middle", x = (int)drag.X, y = (int)drag.Y });
        await Task.Delay(100); Check(_middleEvents == 1, "Actual middle mouse button");
        await RequestAsync(new { operation = "desktop", action = "drag", windowId = id, processId = pid, x = (int)drag.X, y = (int)drag.Y,
            endX = (int)drag.X + 70, endY = (int)drag.Y + 8 });
        await Task.Delay(100); Check(_dragEvents > 0, "Actual synthetic drag start");
        var other = new Window { Title = "Synthetic background fixture", Width = 220, Height = 160, Left = 720, Top = 100 };
        other.Show(); window.Activate(); await Task.Delay(120);
        string otherId = new WindowInteropHelper(other).Handle.ToInt64().ToString();
        CheckError(await RequestAsync(new { operation = "desktop", action = "click", windowId = otherId, processId = pid, x = 10, y = 10 }), "DESKTOP_NOT_FOREGROUND");
        other.Close();
        CheckError(await RequestAsync(new { operation = "desktop", action = "read", windowId = otherId, processId = pid }), "DESKTOP_TARGET_CHANGED");
        string childFolder = Path.Combine(ArtifactFolder, "launched-fixture"); int childPid = 0;
        try
        {
            JsonElement launched = await RequestAsync(new { operation = "desktop", action = "launch", appPath = Environment.ProcessPath,
                args = new[] { "--fixture", childFolder } });
            childPid = launched.GetProperty("processId").GetInt32();
            for (int i = 0; i < 40 && !File.Exists(Path.Combine(childFolder, "ready.json")); i++) await Task.Delay(100);
            Check(File.Exists(Path.Combine(childFolder, "ready.json")), "Launch started only a synthetic GUI executable");
            JsonElement child = JsonDocument.Parse(await File.ReadAllTextAsync(Path.Combine(childFolder, "ready.json"))).RootElement;
            Check(child.GetProperty("processId").GetInt32() == childPid, "Launch PID belongs to the synthetic app");
        }
        finally
        {
            if (childPid > 0) try { using Process process = Process.GetProcessById(childPid); process.Kill(entireProcessTree: true); } catch (ArgumentException) { }
        }
    }

    private static async Task CheckInputExtensionsAsync(Window window)
    {
        string id = new WindowInteropHelper(window).Handle.ToInt64().ToString(); int pid = Environment.ProcessId;
        JsonElement apps = await RequestAsync(new { operation = "desktop", action = "apps" });
        Check(apps.GetProperty("apps").GetArrayLength() <= 128 && apps.GetProperty("source").GetString() == "known-apps-and-registry-app-paths", "Bounded registry application discovery");
        Check(apps.GetProperty("apps").EnumerateArray().All(app => Path.IsPathFullyQualified(app.GetProperty("appPath").GetString()!)), "Discovered application paths are absolute");
        CheckError(await RequestAsync(new { operation = "desktop", action = "type", windowId = id, processId = pid, text = new string('x', 4001) }), "DESKTOP_INVALID_REQUEST");
        Check(Input.Text == "initial fixture", "Overlong input does not change fixture");
        CheckError(await RequestAsync(new { operation = "desktop", action = "read", windowId = id, processId = pid, maxCharacters = 0 }), "DESKTOP_INVALID_REQUEST");
        window.WindowState = WindowState.Minimized;
        await RequestAsync(new { operation = "desktop", action = "activate", windowId = id, processId = pid });
        Check(window.WindowState != WindowState.Minimized, "Activate restores the owned minimized window");
        Point point = DragBox.PointToScreen(new Point(20, DragBox.ActualHeight / 2));
        NativePoint origin = new(); ClientToScreen(new WindowInteropHelper(window).Handle, ref origin);
        JsonElement clicked = await RequestAsync(new { operation = "desktop", action = "click", windowId = id, processId = pid, button = "middle",
            x = (int)point.X - origin.X, y = (int)point.Y - origin.Y });
        await Task.Delay(100);
        Check(clicked.GetProperty("completed").GetBoolean() && _middleEvents == 1, "Actual middle mouse button delivered to owned fixture");
    }

    private static void Check(bool passed, string description) { if (!passed) throw new Exception(description); _checks++; }
    private static void CheckError(JsonElement response, string code) => Check(response.TryGetProperty("error", out JsonElement error)
        && error.GetProperty("code").GetString() == code, "Expected fail-closed code " + code);
    [StructLayout(LayoutKind.Sequential)] private struct NativePoint { public int X, Y; }
    [DllImport("user32.dll")] private static extern bool ClientToScreen(nint window, ref NativePoint point);
}

using System.Text;
using System.Text.Json;

// A protocol fixture only: it never executes the supplied script as a command.
Console.InputEncoding = Encoding.UTF8;
Console.OutputEncoding = new UTF8Encoding(false);
string? input = await Console.In.ReadLineAsync();
if (input is null) return 2;
using var document = JsonDocument.Parse(input);
JsonElement request = document.RootElement;
string operation = request.GetProperty("operation").GetString()!;
if (operation == "host_terminal_capabilities")
{
    Write(new { protocolVersion = 2, boundary = "host-terminal", available = true, sandbox = false,
        processTreeBounded = true, shells = new[] { "cmd", "powershell" }, visibleTerminal = true });
    return 0;
}
string mode = request.GetProperty("script").GetString()!;
string cwd = request.GetProperty("cwd").GetString()!;
if (mode == "not_started")
{
    Write(new { protocolVersion = 2, boundary = "host-terminal", completed = false, outcome = "not_started",
        error = new { code = "HOST_TERMINAL_START_FAILED", message = "Synthetic refusal before any effect." } });
    return 1;
}
await File.AppendAllTextAsync(Path.Combine(cwd, "effects.txt"), "before\n");
if (mode == "lost_before_started") return 17;
Write(new { protocolVersion = 2, boundary = "host-terminal", @event = "host_terminal_started", processId = Environment.ProcessId });

if (mode == "many_console_frames")
{
    for (int sequence = 1; sequence <= 40; sequence++)
        Write(new { protocolVersion = 2, boundary = "host-terminal", @event = "host_terminal_output",
            sequence, stream = "console", replace = true, text = new string('x', 64000) + sequence });
    Complete("final bounded screen", "", "");
    return 0;
}

string delta = "分段中文UTF8输出\n";
string frame = JsonSerializer.Serialize(new { protocolVersion = 2, boundary = "host-terminal", @event = "host_terminal_output",
    sequence = 1, stream = "stdout", delta }, JsonOptions());
// Split inside a multibyte UTF-8 character, not only between complete protocol lines.
byte[] bytes = Encoding.UTF8.GetBytes(frame + "\n");
int split = Array.IndexOf(bytes, (byte)0xe5) + 1;
using (Stream output = Console.OpenStandardOutput())
{
    await output.WriteAsync(bytes.AsMemory(0, split)); await output.FlushAsync();
    await Task.Delay(25);
    await output.WriteAsync(bytes.AsMemory(split)); await output.FlushAsync();
}
if (mode == "partial_lost") return 19;
if (mode == "display_failure")
{
    Task<string?> cancellation = Console.In.ReadLineAsync();
    Task winner = await Task.WhenAny(cancellation, Task.Delay(3000));
    bool cancelled = winner == cancellation && await cancellation is null or "cancel";
    if (!cancelled) await File.AppendAllTextAsync(Path.Combine(cwd, "effects.txt"), "after\n");
    Write(new { protocolVersion = 2, boundary = "host-terminal", completed = !cancelled,
        outcome = cancelled ? "unknown" : "completed", shell = request.GetProperty("shell").GetString(), cwd,
        exitCode = cancelled ? 125 : 0, stdout = delta, stderr = "", cancelled, timedOut = false,
        processId = Environment.ProcessId, activeProcessesAfterExit = 0 });
    return 0;
}
if (mode != "completed_live") return 3;
await Task.Delay(500);
await File.AppendAllTextAsync(Path.Combine(cwd, "effects.txt"), "after\n");
Complete("", delta, "");
return 0;

void Complete(string consoleText, string stdout, string stderr)
{
    bool visible = operation == "host_terminal_visible";
    Write(new { protocolVersion = 2, boundary = "host-terminal", completed = true, outcome = "completed",
        shell = request.GetProperty("shell").GetString(), cwd, exitCode = 0, stdout, stderr,
        processId = Environment.ProcessId, activeProcessesAfterExit = 0, visibleRequested = visible,
        windowObserved = visible, windowId = "12345", windowProcessId = Environment.ProcessId,
        consoleInput = "console", consoleOutput = "console", outputCapture = "console-screen",
        commandCompleted = true, exitCodeObserved = true, consoleText, consoleSnapshotAvailable = true,
        displayHoldMs = 0 });
}

static JsonSerializerOptions JsonOptions() => new(JsonSerializerDefaults.Web)
{
    Encoder = System.Text.Encodings.Web.JavaScriptEncoder.UnsafeRelaxedJsonEscaping
};

static void Write(object value) => Console.WriteLine(JsonSerializer.Serialize(value, JsonOptions()));

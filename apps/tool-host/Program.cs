using System.Text;
using System.Text.Json;

namespace KYNXA.ToolHost;

internal static class Program
{
    internal static readonly JsonSerializerOptions JsonOptions = new(JsonSerializerDefaults.Web);

    private static async Task<int> Main(string[] args)
    {
        if (args.Length == 2 && args[0] == "--host-terminal-visible-worker")
            return await HostTerminalVisibleWorker.RunAsync(args[1]);
        Console.InputEncoding = Encoding.UTF8;
        Console.OutputEncoding = new UTF8Encoding(false);
        bool desktopRequest = false, hostTerminalRequest = false;
        try
        {
            string input = await Console.In.ReadLineAsync()
                ?? throw new SandboxException("SANDBOX_INVALID_REQUEST", "Missing sandbox request.");
            if (input.Length > 128 * 1024) throw new SandboxException("SANDBOX_INVALID_REQUEST", "Sandbox request is too large.");
            using JsonDocument document = JsonDocument.Parse(input);
            string? operation = document.RootElement.TryGetProperty("operation", out JsonElement value) && value.ValueKind == JsonValueKind.String
                ? value.GetString() : null;
            if (operation is "host_terminal_capabilities" or "host_terminal" or "host_terminal_visible")
            {
                hostTerminalRequest = true;
                if (operation == "host_terminal_capabilities")
                    Console.WriteLine(JsonSerializer.Serialize(HostTerminalRunner.Capabilities(), JsonOptions));
                else
                {
                    var terminal = JsonSerializer.Deserialize<HostTerminalRequest>(input, JsonOptions)
                        ?? throw new HostTerminalException("HOST_TERMINAL_INVALID_REQUEST", "Missing host terminal request.");
                    using var cancelTerminal = new CancellationTokenSource();
                    _ = Task.Run(() => MonitorCancellationAsync(cancelTerminal));
                    Console.WriteLine(JsonSerializer.Serialize(await HostTerminalRunner.RunAsync(terminal, cancelTerminal.Token), JsonOptions));
                }
                return 0;
            }
            if (operation is "desktop_capabilities" or "desktop")
            {
                desktopRequest = true;
                if (operation == "desktop_capabilities")
                    Console.WriteLine(JsonSerializer.Serialize(DesktopRunner.Capabilities(), JsonOptions));
                else
                {
                    var desktop = JsonSerializer.Deserialize<DesktopRequest>(input, JsonOptions)
                        ?? throw new DesktopException("DESKTOP_INVALID_REQUEST", "Missing desktop request.");
                    using var cancelDesktop = new CancellationTokenSource();
                    _ = Task.Run(() => MonitorCancellationAsync(cancelDesktop));
                    Console.WriteLine(JsonSerializer.Serialize(await DesktopRunner.RunAsync(desktop, cancelDesktop.Token), JsonOptions));
                }
                return 0;
            }
            var request = JsonSerializer.Deserialize<SandboxRequest>(input, JsonOptions)
                ?? throw new SandboxException("SANDBOX_INVALID_REQUEST", "Missing sandbox request.");
            if (request.Operation == "capabilities")
            {
                Console.WriteLine(JsonSerializer.Serialize(new
                {
                    protocolVersion = 1, available = OperatingSystem.IsWindowsVersionAtLeast(6, 2),
                    sandbox = "appcontainer", commands = new[] { "node", "cmd" }, network = false,
                    workspaceCopy = true, failClosed = true, checksChildToken = true,
                    skillExecution = new { manifestVersion = 1, readOnlyPackage = true, hashChecked = true },
                    limitations = new[] { "cmd is restricted compatibility: echo/type/redirection; DIR may be denied; use filesystem.list/search.", "node --test requires --test-isolation=none." }
                }, JsonOptions));
                return 0;
            }
            if (request.Operation != "run") throw new SandboxException("SANDBOX_INVALID_REQUEST", "Unknown sandbox operation.");
            using var cancellation = new CancellationTokenSource();
            // Console.In is synchronized: its ReadLineAsync may synchronously block before returning.
            _ = Task.Run(() => MonitorCancellationAsync(cancellation));
            var result = await AppContainerRunner.RunAsync(request, cancellation.Token);
            Console.WriteLine(JsonSerializer.Serialize(result, JsonOptions));
            return 0;
        }
        catch (Exception exception)
        {
            if (hostTerminalRequest)
            {
                string terminalCode = exception switch { HostTerminalException failure => failure.Code,
                    OperationCanceledException => "HOST_TERMINAL_CANCELLED", UnauthorizedAccessException => "HOST_TERMINAL_ACCESS_DENIED",
                    JsonException => "HOST_TERMINAL_INVALID_REQUEST", _ => "HOST_TERMINAL_FAILED" };
                bool partial = exception is HostTerminalException started && started.ProcessStarted;
                Console.WriteLine(JsonSerializer.Serialize(new { protocolVersion = 1, boundary = "host-terminal", completed = false,
                    outcome = partial ? "unknown" : "not_started", error = new { code = terminalCode, message = exception.Message,
                    partial, outcome = partial ? "unknown" : "not_started" } }, JsonOptions));
                return 1;
            }
            if (desktopRequest)
            {
                string desktopCode = exception switch { DesktopException failure => failure.Code,
                    OperationCanceledException => "DESKTOP_CANCELLED", TimeoutException => "DESKTOP_TIMEOUT",
                    UnauthorizedAccessException => "DESKTOP_ACCESS_DENIED", JsonException => "DESKTOP_INVALID_REQUEST", _ => "DESKTOP_FAILED" };
                int delivered = exception is DesktopException partial ? partial.DeliveredInputEvents : 0;
                Console.WriteLine(JsonSerializer.Serialize(new { error = new { code = desktopCode, message = exception.Message,
                    partial = delivered > 0, deliveredInputEvents = delivered } }, JsonOptions));
                return 1;
            }
            string code = exception is SandboxException sandbox ? sandbox.Code : "SANDBOX_START_FAILED";
            Console.WriteLine(JsonSerializer.Serialize(new { error = new { code, message = exception.Message } }, JsonOptions));
            return 1;
        }
    }

    private static async Task MonitorCancellationAsync(CancellationTokenSource cancellation)
    {
        try
        {
            // EOF also cancels: a terminated gateway cannot leave a sandbox worker running.
            string? line = await Console.In.ReadLineAsync();
            if (line is null or "cancel") cancellation.Cancel();
        }
        catch (IOException) { cancellation.Cancel(); }
        catch (ObjectDisposedException) { }
    }
}

internal sealed record SandboxRequest
{
    public string Operation { get; init; } = "run";
    public string? WorkspaceRoot { get; init; }
    public string? NodeExecutable { get; init; }
    public string? Command { get; init; }
    public string[] Args { get; init; } = [];
    public string[] ExcludedRoots { get; init; } = [];
    public int TimeoutMs { get; init; } = 30000;
    public bool TrustedManagedWorkspace { get; init; }
    public string? ConversationWorkspaceHome { get; init; }
    public SandboxSkill? Skill { get; init; }
}

internal sealed class SandboxException(string code, string message) : Exception(message)
{
    public string Code { get; } = code;
}

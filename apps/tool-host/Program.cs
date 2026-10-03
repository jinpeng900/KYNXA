using System.Text;
using System.Text.Json;

namespace KYNXA.ToolHost;

internal static class Program
{
    internal static readonly JsonSerializerOptions JsonOptions = new(JsonSerializerDefaults.Web);

    private static async Task<int> Main()
    {
        Console.InputEncoding = Encoding.UTF8;
        Console.OutputEncoding = new UTF8Encoding(false);
        try
        {
            string input = await Console.In.ReadLineAsync()
                ?? throw new SandboxException("SANDBOX_INVALID_REQUEST", "Missing sandbox request.");
            if (input.Length > 128 * 1024) throw new SandboxException("SANDBOX_INVALID_REQUEST", "Sandbox request is too large.");
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
    public SandboxSkill? Skill { get; init; }
}

internal sealed class SandboxException(string code, string message) : Exception(message)
{
    public string Code { get; } = code;
}

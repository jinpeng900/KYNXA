namespace KYNXA.ToolHost;

internal sealed record HostTerminalRequest
{
    internal const int MaximumScriptCharacters = 16384;

    public string Operation { get; init; } = "host_terminal";
    public string? Shell { get; init; }
    public string? Script { get; init; }
    public string? Cwd { get; init; }
    public int TimeoutMs { get; init; } = 30000;
    public bool Visible { get; init; }
    public int? KeepOpenMs { get; init; }
}

internal sealed class HostTerminalException(string code, string message, bool processStarted = false) : Exception(message)
{
    public string Code { get; } = code;
    public bool ProcessStarted { get; } = processStarted;
}

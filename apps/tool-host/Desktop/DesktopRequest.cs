namespace KYNXA.ToolHost;

internal sealed record DesktopRequest
{
    public string Operation { get; init; } = "desktop";
    public string? Action { get; init; }
    public string? WindowId { get; init; }
    public int? ProcessId { get; init; }
    public string? AppPath { get; init; }
    public string[] Args { get; init; } = [];
    public bool Background { get; init; } = true;
    public string? Mode { get; init; }
    public int? Width { get; init; }
    public int? Height { get; init; }
    public DesktopRegion? Crop { get; init; }
    public DesktopRegion? Region { get; init; }
    public string? ElementId { get; init; }
    public int TimeoutMs { get; init; } = 3000;
    public int? X { get; init; }
    public int? Y { get; init; }
    public int? EndX { get; init; }
    public int? EndY { get; init; }
    public int Delta { get; init; }
    public string Direction { get; init; } = "vertical";
    public string Button { get; init; } = "left";
    public string? Text { get; init; }
    public string? Key { get; init; }
    public int MaxCharacters { get; init; } = 16000;
    public int MaxElements { get; init; } = 200;
}

internal sealed record DesktopWindow(string WindowId, int ProcessId, string Title, string ProcessName,
    string? ExecutablePath, int ClientWidth, int ClientHeight, bool IsForeground, bool IsMinimized, bool IsResponding);

internal sealed record DesktopRegion(int X, int Y, int Width, int Height);

internal sealed class DesktopException(string code, string message, int deliveredInputEvents = 0) : Exception(message)
{
    public string Code { get; } = code;
    public int DeliveredInputEvents { get; } = deliveredInputEvents;
}

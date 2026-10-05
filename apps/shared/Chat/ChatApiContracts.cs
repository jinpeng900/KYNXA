namespace KYNXA.Contracts;

public static class ChatPermissionModes
{
    public const string Ask = "ask";
    public const string Smart = "smart";
    public const string Full = "full";
    public static bool IsSupported(string? mode) => mode is Ask or Smart or Full;
}

public sealed record ChatRequest(Guid ConversationId, string Message, string? Model = null,
    string PermissionMode = ChatPermissionModes.Ask, string? Provider = null, Guid? RequestId = null, Guid? UserMessageId = null);
public sealed record ChatReply(Guid ConversationId, Guid RequestId, string Role, string Content, DateTimeOffset CreatedAt,
    long DurationMs = 0);

public sealed record ChatStreamEvent(string Type, Guid ConversationId, Guid RequestId, DateTimeOffset CreatedAt,
    string? Delta = null, string? Content = null, string? Reasoning = null, string? Error = null, ToolActivity? Tool = null,
    AssistantSegment? Segment = null, string? SegmentId = null, AssistantSegment[]? AssistantSegments = null,
    int? ToolStreamProtocol = null, long DurationMs = 0, HostTerminalOutput? Terminal = null);

public static class ChatDurationRules
{
    public const long MaximumDurationMs = 9007199254740991;
    public static bool IsValid(long value) => value is >= 0 and <= MaximumDurationMs;
}
